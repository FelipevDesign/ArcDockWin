//! Painel de rede (clique no ícone de rede da topbar): liga/desliga o Wi‑Fi, rede conectada,
//! redes disponíveis e conexão (perfil salvo, rede aberta ou senha WPA2/WPA3‑Personal).
//!
//! A WlanAPI avisa (numa thread dela) o fim da varredura e o resultado da conexão; o callback
//! posta [`WM_WIFI`] na janela da topbar, que repassa ao painel com `panel::notify`. A sessão
//! e o registro das notificações vivem só enquanto o painel está aberto.

use std::ffi::c_void;

use windows::Devices::Radios::{Radio, RadioAccessStatus, RadioKind, RadioState};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::NetworkManagement::WiFi::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SW_SHOWNORMAL, WM_APP};
use windows::core::{GUID, HSTRING, PCWSTR, w};

use crate::panel::{self, Event, PanelContent, Widget, glyph};
use crate::status::{self, Net};

/// Postada na janela da topbar. wParam: tipo (byte baixo) e, em `CONNECT`, o código de
/// motivo da WlanAPI nos bits de cima.
pub const WM_WIFI: u32 = WM_APP + 40;
const LIST: usize = 1;
const CONNECT: usize = 2;
const RADIO: usize = 3;
const RADIO_FAIL: usize = 4;

const TOGGLE: u32 = 1;
const REFRESH: u32 = 2;
const SETTINGS: u32 = 3;
const DISCONNECT: u32 = 4;
const JOIN: u32 = 5;
/// Linhas de rede: NET + índice em `nets`.
const NET: u32 = 100;
/// Campo de senha: FIELD + número da abertura (texto novo a cada rede aberta).
const FIELD: u32 = 1000;
const MAX_NETS: usize = 8;

/// Wi‑Fi com 1 a 4 barras (Segoe Fluent Icons).
const BARS: [u16; 4] = [0xE872, 0xE873, 0xE874, 0xE701];
const ETHERNET: u16 = 0xE839;
const NO_NET: u16 = 0xF384;

struct Network {
    ssid: Vec<u8>,
    name: String,
    signal: u32,
    secure: bool,
    connected: bool,
    /// Perfil salvo (nome, sem o zero final).
    profile: Option<Vec<u16>>,
    auth: DOT11_AUTH_ALGORITHM,
    cipher: DOT11_CIPHER_ALGORITHM,
}

pub struct WifiPanel {
    topbar: isize,
    handle: HANDLE,
    iface: Option<GUID>,
    /// Rádio ligado (`None` = não deu para ler).
    radio: Option<bool>,
    /// Falso depois que ligar/desligar falhou pelos dois caminhos: interruptor escondido.
    can_toggle: bool,
    nets: Vec<Network>,
    scanning: bool,
    /// Rede com a senha (ou o "Desconectar") aberta embaixo.
    expanded: Option<Vec<u8>>,
    field_seq: u32,
    password: String,
    connecting: Option<Vec<u8>>,
    /// Perfil criado agora para `connecting`: apagado se a conexão falhar.
    created: Option<Vec<u16>>,
    /// Mensagem de erro sob uma rede.
    result: Option<(Vec<u8>, &'static str)>,
    /// Estado da rede quando não há Wi‑Fi.
    wired: Net,
}

fn post(hwnd: isize, kind: usize, reason: u32) {
    unsafe {
        let _ = PostMessageW(
            Some(HWND(hwnd as *mut c_void)),
            WM_WIFI,
            WPARAM(kind | (reason as usize) << 8),
            LPARAM(0),
        );
    }
}

/// Callback da WlanAPI (thread dela): só traduz e posta para a topbar.
#[allow(non_upper_case_globals)]
unsafe extern "system" fn on_wlan(data: *mut L2_NOTIFICATION_DATA, ctx: *mut c_void) {
    let Some(d) = (unsafe { data.as_ref() }) else { return };
    let code = d.NotificationCode as i32;
    let (kind, reason) = if d.NotificationSource == WLAN_NOTIFICATION_SOURCE_ACM {
        match WLAN_NOTIFICATION_ACM(code) {
            wlan_notification_acm_scan_complete
            | wlan_notification_acm_scan_fail
            | wlan_notification_acm_scan_list_refresh
            | wlan_notification_acm_disconnected
            | wlan_notification_acm_interface_arrival
            | wlan_notification_acm_interface_removal => (LIST, 0),
            wlan_notification_acm_connection_complete | wlan_notification_acm_connection_attempt_fail => {
                if d.pData.is_null() || (d.dwDataSize as usize) < size_of::<WLAN_CONNECTION_NOTIFICATION_DATA>() {
                    return;
                }
                let c = unsafe { &*(d.pData as *const WLAN_CONNECTION_NOTIFICATION_DATA) };
                (CONNECT, c.wlanReasonCode)
            }
            _ => return,
        }
    } else if d.NotificationSource == WLAN_NOTIFICATION_SOURCE_MSM
        && WLAN_NOTIFICATION_MSM(code) == wlan_notification_msm_radio_state_change
    {
        (RADIO, 0)
    } else {
        return;
    };
    post(ctx as isize, kind, reason);
}

fn wide_name(raw: &[u16]) -> Vec<u16> {
    raw[..raw.iter().position(|&c| c == 0).unwrap_or(raw.len())].to_vec()
}

fn zero(mut s: Vec<u16>) -> Vec<u16> {
    s.push(0);
    s
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Autenticação e criptografia do perfil para a rede; `None` = não dá para criar aqui
/// (Enterprise, WEP...).
fn security(n: &Network) -> Option<(&'static str, &'static str)> {
    if !n.secure {
        return Some(("open", "none"));
    }
    let auth = match n.auth {
        DOT11_AUTH_ALGO_RSNA_PSK => "WPA2PSK",
        DOT11_AUTH_ALGO_WPA3_SAE => "WPA3SAE",
        DOT11_AUTH_ALGO_WPA_PSK => "WPAPSK",
        _ => return None,
    };
    let enc = if n.cipher == DOT11_CIPHER_ALGO_TKIP { "TKIP" } else { "AES" };
    Some((auth, enc))
}

/// Perfil XML (nome = SSID). `key` só para redes protegidas.
fn profile_xml(n: &Network, auth: &str, enc: &str, key: Option<&str>) -> String {
    let name = xml_escape(&n.name);
    let hex: String = n.ssid.iter().map(|b| format!("{b:02X}")).collect();
    let shared = key.map_or(String::new(), |k| {
        format!(
            "<sharedKey><keyType>passPhrase</keyType><protected>false</protected><keyMaterial>{}</keyMaterial></sharedKey>",
            xml_escape(k)
        )
    });
    format!(
        "<?xml version=\"1.0\"?><WLANProfile xmlns=\"http://www.microsoft.com/networking/WLAN/profile/v1\">\
<name>{name}</name><SSIDConfig><SSID><hex>{hex}</hex><name>{name}</name></SSID></SSIDConfig>\
<connectionType>ESS</connectionType><connectionMode>auto</connectionMode><MSM><security>\
<authEncryption><authentication>{auth}</authentication><encryption>{enc}</encryption><useOneX>false</useOneX></authEncryption>\
{shared}</security></MSM></WLANProfile>"
    )
}

/// Liga/desliga o rádio: WlanAPI (pode exigir administrador) e, se falhar, Windows.Devices.Radios.
fn set_radio_wlan(iface: GUID, on: bool) -> bool {
    let state = if on { dot11_radio_state_on } else { dot11_radio_state_off };
    let (mut version, mut h) = (0u32, HANDLE::default());
    if unsafe { WlanOpenHandle(2, None, &mut version, &mut h) } != 0 {
        return false;
    }
    let mut ok = false;
    unsafe {
        let (mut size, mut data) = (0u32, std::ptr::null_mut::<c_void>());
        let phys = if WlanQueryInterface(h, &iface, wlan_intf_opcode_radio_state, None, &mut size, &mut data, None) == 0
            && !data.is_null()
        {
            let n = (*(data as *const WLAN_RADIO_STATE)).dwNumberOfPhys.min(64);
            WlanFreeMemory(data);
            n
        } else {
            1
        };
        for i in 0..phys {
            let s = WLAN_PHY_RADIO_STATE { dwPhyIndex: i, dot11SoftwareRadioState: state, dot11HardwareRadioState: state };
            if WlanSetInterface(
                h,
                &iface,
                wlan_intf_opcode_radio_state,
                size_of::<WLAN_PHY_RADIO_STATE>() as u32,
                &s as *const _ as *const c_void,
                None,
            ) == 0
            {
                ok = true;
            }
        }
        WlanCloseHandle(h, None);
    }
    ok
}

fn set_radio_winrt(on: bool) -> bool {
    let run = || -> windows::core::Result<bool> {
        let radios = Radio::GetRadiosAsync()?.join()?;
        let mut ok = false;
        for i in 0..radios.Size()? {
            let r = radios.GetAt(i)?;
            if r.Kind()? == RadioKind::WiFi {
                let state = if on { RadioState::On } else { RadioState::Off };
                ok |= r.SetStateAsync(state)?.join()? == RadioAccessStatus::Allowed;
            }
        }
        Ok(ok)
    };
    unsafe {
        let init = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let ok = run().unwrap_or(false);
        if init {
            CoUninitialize();
        }
        ok
    }
}

impl WifiPanel {
    pub fn new() -> Self {
        let topbar = crate::topbar::hwnd().0 as isize;
        let mut p = Self {
            topbar,
            handle: HANDLE::default(),
            iface: None,
            radio: None,
            can_toggle: true,
            nets: Vec::new(),
            scanning: false,
            expanded: None,
            field_seq: 0,
            password: String::new(),
            connecting: None,
            created: None,
            result: None,
            wired: Net::None,
        };
        unsafe {
            let mut version = 0u32;
            if WlanOpenHandle(2, None, &mut version, &mut p.handle) != 0 {
                p.handle = HANDLE::default();
            }
            if !p.handle.is_invalid() {
                let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
                if WlanEnumInterfaces(p.handle, None, &mut list) == 0 && !list.is_null() {
                    if (*list).dwNumberOfItems > 0 {
                        p.iface = Some((*list).InterfaceInfo[0].InterfaceGuid);
                    }
                    WlanFreeMemory(list as *const c_void);
                }
            }
            match p.iface {
                Some(iface) => {
                    WlanRegisterNotification(
                        p.handle,
                        WLAN_NOTIFICATION_SOURCE_ACM | WLAN_NOTIFICATION_SOURCE_MSM,
                        true,
                        Some(on_wlan),
                        Some(topbar as *const c_void),
                        None,
                        None,
                    );
                    p.read_radio();
                    p.load();
                    p.scanning = WlanScan(p.handle, &iface, None, None, None) == 0;
                }
                None => {
                    // Sem Wi‑Fi: a sessão não serve para nada.
                    p.close_handle();
                    p.wired = status::network(&status::Wlan::open());
                }
            }
        }
        p
    }

    /// Para a Central de Controle: (tem Wi‑Fi, rádio ligado se dá para alternar, rede conectada).
    pub fn summary(&self) -> (bool, Option<bool>, Option<String>) {
        let name = self.nets.iter().find(|n| n.connected).map(|n| n.name.clone());
        (self.iface.is_some(), self.radio.filter(|_| self.can_toggle), name)
    }

    /// Liga/desliga o rádio como o interruptor do painel; `true` = reconstruir.
    pub fn set_radio(&mut self, on: bool) -> bool {
        self.event(Event::Toggle(TOGGLE, on))
    }

    fn close_handle(&mut self) {
        if !self.handle.is_invalid() {
            unsafe {
                WlanRegisterNotification(self.handle, WLAN_NOTIFICATION_SOURCE_NONE, true, None, None, None, None);
                WlanCloseHandle(self.handle, None);
            }
            self.handle = HANDLE::default();
        }
    }

    fn read_radio(&mut self) {
        let Some(iface) = self.iface else { return };
        unsafe {
            let (mut size, mut data) = (0u32, std::ptr::null_mut::<c_void>());
            if WlanQueryInterface(self.handle, &iface, wlan_intf_opcode_radio_state, None, &mut size, &mut data, None) != 0
                || data.is_null()
            {
                self.radio = None;
                return;
            }
            let st = &*(data as *const WLAN_RADIO_STATE);
            let n = (st.dwNumberOfPhys as usize).min(64);
            self.radio = Some(st.PhyRadioState[..n].iter().any(|p| {
                p.dot11SoftwareRadioState == dot11_radio_state_on && p.dot11HardwareRadioState == dot11_radio_state_on
            }));
            WlanFreeMemory(data);
        }
    }

    /// Redes visíveis, sem SSID repetido: conectada primeiro, depois por sinal.
    fn load(&mut self) {
        self.nets.clear();
        let Some(iface) = self.iface else { return };
        if self.radio == Some(false) {
            return;
        }
        unsafe {
            let mut list: *mut WLAN_AVAILABLE_NETWORK_LIST = std::ptr::null_mut();
            if WlanGetAvailableNetworkList(self.handle, &iface, 0, None, &mut list) != 0 || list.is_null() {
                return;
            }
            let items = std::slice::from_raw_parts((*list).Network.as_ptr(), (*list).dwNumberOfItems as usize);
            for a in items {
                let len = (a.dot11Ssid.uSSIDLength as usize).min(32);
                if len == 0 || a.dot11BssType != dot11_BSS_type_infrastructure {
                    continue;
                }
                let ssid = a.dot11Ssid.ucSSID[..len].to_vec();
                let connected = a.dwFlags & WLAN_AVAILABLE_NETWORK_CONNECTED != 0;
                let profile = (a.dwFlags & WLAN_AVAILABLE_NETWORK_HAS_PROFILE != 0)
                    .then(|| wide_name(&a.strProfileName))
                    .filter(|p| !p.is_empty());
                if let Some(n) = self.nets.iter_mut().find(|n| n.ssid == ssid) {
                    n.signal = n.signal.max(a.wlanSignalQuality);
                    n.connected |= connected;
                    // O perfil em uso vale mais que outro perfil da mesma rede.
                    if profile.is_some() && (n.profile.is_none() || connected) {
                        n.profile = profile;
                    }
                    continue;
                }
                self.nets.push(Network {
                    name: String::from_utf8_lossy(&ssid).into_owned(),
                    ssid,
                    signal: a.wlanSignalQuality,
                    secure: a.bSecurityEnabled.as_bool(),
                    connected,
                    profile,
                    auth: a.dot11DefaultAuthAlgorithm,
                    cipher: a.dot11DefaultCipherAlgorithm,
                });
            }
            WlanFreeMemory(list as *const c_void);
        }
        self.nets.sort_by(|a, b| b.connected.cmp(&a.connected).then(b.signal.cmp(&a.signal)));
        self.nets.truncate(MAX_NETS);
    }

    fn scan(&mut self) {
        if let Some(iface) = self.iface {
            self.scanning = unsafe { WlanScan(self.handle, &iface, None, None, None) } == 0;
        }
    }

    fn fail(&mut self, ssid: Vec<u8>, msg: &'static str) {
        self.connecting = None;
        self.result = Some((ssid, msg));
    }

    /// Conecta pelo perfil `name` (sem zero final).
    fn connect_profile(&mut self, ssid: Vec<u8>, name: Vec<u16>) {
        let Some(iface) = self.iface else { return };
        let name = zero(name);
        let params = WLAN_CONNECTION_PARAMETERS {
            wlanConnectionMode: wlan_connection_mode_profile,
            strProfile: PCWSTR(name.as_ptr()),
            dot11BssType: dot11_BSS_type_infrastructure,
            ..Default::default()
        };
        self.result = None;
        self.expanded = None;
        if unsafe { WlanConnect(self.handle, &iface, &params, None) } == 0 {
            self.connecting = Some(ssid);
        } else {
            self.drop_created();
            self.fail(ssid, "Falha ao conectar");
        }
    }

    /// Cria (ou sobrescreve) o perfil da rede `i` e conecta.
    fn connect_new(&mut self, i: usize, key: Option<&str>) {
        let Some(iface) = self.iface else { return };
        let Some(n) = self.nets.get(i) else { return };
        let ssid = n.ssid.clone();
        let Some((auth, enc)) = security(n) else { return };
        let xml = HSTRING::from(profile_xml(n, auth, enc, key));
        let name: Vec<u16> = n.name.encode_utf16().collect();
        let mut reason = 0u32;
        let r = unsafe { WlanSetProfile(self.handle, &iface, 0, &xml, PCWSTR::null(), true, None, &mut reason) };
        self.password.clear();
        if r != 0 {
            // Motivos do módulo de segurança: chave inválida (tamanho, caracteres).
            let wrong = (WLAN_REASON_CODE_MSMSEC_BASE..WLAN_REASON_CODE_MSMSEC_BASE + 0x10000).contains(&reason);
            self.expanded = None;
            self.fail(ssid, if wrong { "Senha incorreta" } else { "Falha ao conectar" });
            return;
        }
        self.created = Some(name.clone());
        self.connect_profile(ssid, name);
    }

    /// Apaga o perfil criado agora (a conexão falhou: não deixa senha errada salva).
    fn drop_created(&mut self) {
        if let (Some(name), Some(iface)) = (self.created.take(), self.iface) {
            let name = zero(name);
            unsafe { WlanDeleteProfile(self.handle, &iface, PCWSTR(name.as_ptr()), None) };
        }
    }

    fn click_net(&mut self, i: usize) {
        let Some(n) = self.nets.get(i) else { return };
        let ssid = n.ssid.clone();
        let open = self.expanded.as_ref() == Some(&ssid);
        if n.connected {
            self.expanded = if open { None } else { Some(ssid) };
            return;
        }
        if let Some(profile) = n.profile.clone() {
            self.connect_profile(ssid, profile);
        } else if !n.secure {
            self.connect_new(i, None);
        } else if security(n).is_some() {
            self.expanded = if open { None } else { Some(ssid) };
            self.field_seq += 1;
            self.password.clear();
            self.result = None;
        } else {
            self.fail(ssid, "Use as Configurações de rede");
        }
    }

    fn join_expanded(&mut self, key: &str) {
        let Some(i) = self.expanded.as_ref().and_then(|s| self.nets.iter().position(|n| &n.ssid == s)) else { return };
        let key = key.to_string();
        self.connect_new(i, Some(&key));
    }

    fn connected_result(&mut self, reason: u32) {
        let Some(ssid) = self.connecting.take() else { return };
        if reason == 0 {
            self.created = None;
            self.result = None;
            return;
        }
        self.drop_created();
        let wrong = (WLAN_REASON_CODE_MSMSEC_BASE..WLAN_REASON_CODE_MSMSEC_BASE + 0x10000).contains(&reason)
            || reason == WLAN_REASON_CODE_KEY_MISMATCH;
        // Senha errada: reabre o campo para tentar de novo.
        if wrong && self.nets.iter().any(|n| n.ssid == ssid && n.secure && security(n).is_some()) {
            self.expanded = Some(ssid.clone());
            self.field_seq += 1;
        }
        self.fail(ssid, if wrong { "Senha incorreta" } else { "Falha ao conectar" });
    }

    fn row(&self, i: usize, n: &Network) -> Widget {
        let level = ((n.signal / 25).min(3)) as usize;
        let sub = match &self.result {
            Some((s, msg)) if *s == n.ssid => msg,
            _ if self.connecting.as_ref() == Some(&n.ssid) => "Conectando…",
            _ if n.connected => "Conectado",
            _ if n.profile.is_some() => "Salva",
            _ if n.secure => "Protegida",
            _ => "Aberta",
        };
        let id = NET + i as u32;
        let (glyph, text, sub) = (Some(BARS[level]), n.name.clone(), Some(sub.to_string()));
        if n.secure {
            Widget::RowBadge { id, glyph, text, sub, selected: n.connected, badge: glyph::LOCK }
        } else {
            Widget::Row { id, glyph, text, sub, selected: n.connected }
        }
    }

    /// Senha (ou "Desconectar") sob a rede aberta.
    fn below(&self, n: &Network, out: &mut Vec<Widget>) {
        if self.expanded.as_ref() != Some(&n.ssid) {
            return;
        }
        if n.connected {
            out.push(Widget::Button { id: DISCONNECT, text: "Desconectar".into(), primary: false });
        } else {
            out.push(Widget::Field { id: FIELD + self.field_seq, placeholder: "Senha".into(), password: true });
            out.push(Widget::Button { id: JOIN, text: "Conectar".into(), primary: true });
        }
    }
}

impl Drop for WifiPanel {
    fn drop(&mut self) {
        self.close_handle();
    }
}

impl PanelContent for WifiPanel {
    fn build(&mut self) -> Vec<Widget> {
        let mut out = Vec::new();
        if self.iface.is_none() {
            out.push(Widget::Title("Rede".into()));
            out.push(match self.wired {
                Net::None => Widget::Row {
                    id: 0,
                    glyph: Some(NO_NET),
                    text: "Sem conexão".into(),
                    sub: None,
                    selected: false,
                },
                _ => Widget::Row {
                    id: 0,
                    glyph: Some(ETHERNET),
                    text: "Ethernet".into(),
                    sub: Some("Conectado".into()),
                    selected: true,
                },
            });
        } else {
            out.push(Widget::Title("Wi‑Fi".into()));
            if let Some(on) = self.radio.filter(|_| self.can_toggle) {
                out.push(Widget::Toggle { id: TOGGLE, text: if on { "Ligado" } else { "Desligado" }.into(), on, icon: None });
            }
            if self.radio != Some(false) {
                let split = self.nets.iter().take_while(|n| n.connected).count();
                for (i, n) in self.nets[..split].iter().enumerate() {
                    out.push(self.row(i, n));
                    self.below(n, &mut out);
                }
                out.push(Widget::Separator);
                out.push(Widget::Heading { text: "Redes disponíveis".into(), buttons: vec![(glyph::REFRESH, REFRESH)] });
                for (i, n) in self.nets.iter().enumerate().skip(split) {
                    out.push(self.row(i, n));
                    self.below(n, &mut out);
                }
                if self.nets.len() == split {
                    let t = if self.scanning { "Procurando redes…" } else { "Nenhuma rede encontrada." };
                    out.push(Widget::Text(t.into()));
                }
            }
        }
        out.push(Widget::Separator);
        out.push(Widget::Row {
            id: SETTINGS,
            glyph: Some(glyph::SETTINGS),
            text: "Configurações de rede".into(),
            sub: None,
            selected: false,
        });
        out
    }

    fn event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Toggle(TOGGLE, on) => {
                let Some(iface) = self.iface else { return false };
                self.radio = Some(on);
                if !on {
                    self.nets.clear();
                    self.expanded = None;
                }
                let hwnd = self.topbar;
                std::thread::spawn(move || {
                    let ok = set_radio_wlan(iface, on) || set_radio_winrt(on);
                    post(hwnd, if ok { RADIO } else { RADIO_FAIL }, 0);
                });
                true
            }
            Event::Click(REFRESH) => {
                self.scan();
                self.load();
                true
            }
            Event::Click(SETTINGS) => {
                unsafe { ShellExecuteW(None, w!("open"), w!("ms-settings:network"), None, None, SW_SHOWNORMAL) };
                panel::close();
                false
            }
            Event::Click(DISCONNECT) => {
                if let Some(iface) = self.iface {
                    unsafe { WlanDisconnect(self.handle, &iface, None) };
                }
                self.expanded = None;
                true
            }
            Event::Click(JOIN) => {
                let key = std::mem::take(&mut self.password);
                self.join_expanded(&key);
                true
            }
            Event::Text(_, t) => {
                self.password = t.to_string();
                false
            }
            Event::Submit(_, t) => {
                self.password.clear();
                self.join_expanded(t);
                true
            }
            Event::Click(id) if id >= NET => {
                self.click_net((id - NET) as usize);
                true
            }
            _ => false,
        }
    }

    fn notify(&mut self, msg: u32, wparam: usize) -> bool {
        if msg != WM_WIFI || self.iface.is_none() {
            return false;
        }
        match wparam & 0xff {
            LIST => {
                self.scanning = false;
                self.load();
            }
            CONNECT => {
                self.connected_result((wparam >> 8) as u32);
                self.load();
            }
            RADIO => {
                let was = self.radio;
                self.read_radio();
                // Ao religar, procura as redes de novo.
                if was == Some(false) && self.radio == Some(true) {
                    self.scan();
                }
                self.load();
            }
            RADIO_FAIL => {
                // Nenhum caminho conseguiu: esconde o interruptor.
                self.can_toggle = false;
                self.read_radio();
                self.load();
            }
            _ => return false,
        }
        true
    }
}
