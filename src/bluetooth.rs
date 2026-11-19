//! Bluetooth: estado do rádio para a topbar e o painel (dispositivos pareados, conectar e
//! desconectar). Tudo que é assíncrono ou demorado roda em threads do MTA, que avisam a
//! janela da topbar com `WM_BT_RADIO` / `WM_BT_PANEL`; a topbar repassa a `panel::notify`.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use windows::Devices::Bluetooth::{BluetoothConnectionStatus, BluetoothDevice, BluetoothLEDevice};
use windows::Devices::Enumeration::DeviceInformation;
use windows::Devices::Radios::{Radio, RadioAccessStatus, RadioKind, RadioState};
use windows::Foundation::TypedEventHandler;
use windows::Win32::Devices::Bluetooth::{
    BLUETOOTH_DEVICE_INFO, BLUETOOTH_FIND_RADIO_PARAMS, BLUETOOTH_SERVICE_DISABLE, BLUETOOTH_SERVICE_ENABLE,
    BluetoothEnumerateInstalledServices, BluetoothFindFirstRadio, BluetoothFindRadioClose, BluetoothGetDeviceInfo,
    BluetoothSetServiceState,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::CoIncrementMTAUsage;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SW_SHOWNORMAL, WM_APP};
use windows::core::{GUID, HSTRING, w};

use crate::panel::{self, Event, PanelContent, Widget, glyph};

/// Estado do rádio, para a topbar: wParam 1 = desligado, 2 = ligado (sem rádio, nada é postado).
pub const WM_BT_RADIO: u32 = WM_APP + 30;
/// Uma thread do painel terminou (lista carregada ou conexão trocada).
pub const WM_BT_PANEL: u32 = WM_APP + 31;

/// Glifos da Segoe Fluent Icons por tipo de dispositivo.
const G_HEADPHONE: u16 = 0xE7F6;
const G_MOUSE: u16 = 0xE962;
const G_KEYBOARD: u16 = 0xE765;
const G_PHONE: u16 = 0xE8EA;

/// Sem rolagem no painel: no máximo isso de dispositivos.
const MAX_DEVICES: usize = 8;

/// Serviços (UUID16 sobre a base do Bluetooth) ligados/desligados para conectar e desconectar.
const fn uuid16(n: u32) -> GUID {
    GUID::from_u128(((n as u128) << 96) | 0x0000_1000_8000_0080_5F9B_34FB)
}
const A2DP_SINK: GUID = uuid16(0x110B);
const HANDSFREE: GUID = uuid16(0x111E);
const HEADSET: GUID = uuid16(0x1108);
const AVRCP: GUID = uuid16(0x110E);
const AVRCP_TARGET: GUID = uuid16(0x110C);
const HID: GUID = uuid16(0x1124);
const KNOWN: [GUID; 6] = [A2DP_SINK, HANDSFREE, HEADSET, AVRCP, AVRCP_TARGET, HID];

/// Janela da topbar (destino das mensagens).
static TOPBAR: AtomicIsize = AtomicIsize::new(0);
/// Rádio Bluetooth (vive o processo inteiro, com o evento StateChanged registrado).
static RADIO: Mutex<Option<Radio>> = Mutex::new(None);

fn post(msg: u32, wp: usize) {
    let h = TOPBAR.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), msg, WPARAM(wp), LPARAM(0));
        }
    }
}

/// Mantém o MTA vivo para as threads de trabalho e para o rádio guardado em `RADIO`.
fn mta() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let _ = CoIncrementMTAUsage();
    });
}

fn spawn(f: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new().stack_size(256 * 1024).spawn(move || {
        mta();
        f();
    });
}

fn radio_on(r: &Radio) -> bool {
    r.State() == Ok(RadioState::On)
}

/// Procura o rádio Bluetooth e avisa a topbar agora e a cada mudança de estado (evento, sem
/// consulta periódica).
pub fn watch(topbar: HWND) {
    TOPBAR.store(topbar.0 as isize, Ordering::Relaxed);
    spawn(|| {
        let Ok(radios) = Radio::GetRadiosAsync().and_then(|op| op.join()) else { return };
        let Some(radio) = radios.into_iter().find(|r| r.Kind() == Ok(RadioKind::Bluetooth)) else { return };
        let handler = TypedEventHandler::<Radio, windows::core::IInspectable>::new(|r, _| {
            if let Ok(r) = r.ok() {
                post(WM_BT_RADIO, if radio_on(r) { 2 } else { 1 });
            }
            Ok(())
        });
        let _ = radio.StateChanged(&handler);
        post(WM_BT_RADIO, if radio_on(&radio) { 2 } else { 1 });
        if let Ok(mut g) = RADIO.lock() {
            *g = Some(radio);
        }
    });
}

fn radio() -> Option<Radio> {
    RADIO.lock().ok().and_then(|g| g.clone())
}

/// Liga ou desliga o rádio (pede acesso antes). O evento StateChanged atualiza a topbar e o
/// painel; se não deu, reenvia o estado atual para desfazer o interruptor.
fn set_radio(on: bool) {
    spawn(move || {
        let Some(r) = radio() else { return };
        let _ = Radio::RequestAccessAsync().and_then(|op| op.join());
        let state = if on { RadioState::On } else { RadioState::Off };
        let ok = r.SetStateAsync(state).and_then(|op| op.join()) == Ok(RadioAccessStatus::Allowed);
        if !ok {
            post(WM_BT_RADIO, if radio_on(&r) { 2 } else { 1 });
        }
    });
}

#[derive(Clone)]
struct Device {
    id: HSTRING,
    name: String,
    glyph: u16,
    /// Bluetooth LE: não há como conectar/desconectar por aqui.
    le: bool,
    audio: bool,
    address: u64,
    connected: bool,
    busy: bool,
    unsupported: bool,
}

/// Tipo pela classe do dispositivo (clássico): serviço maior e, em periféricos, o menor.
fn classic_glyph(raw: u32) -> (u16, bool) {
    match (raw >> 8) & 0x1F {
        0x02 => (G_PHONE, false),
        0x04 => (G_HEADPHONE, true),
        0x05 if raw & 0xC0 == 0x80 => (G_MOUSE, false),
        0x05 if raw & 0x40 != 0 => (G_KEYBOARD, false),
        _ => (glyph::BLUETOOTH, false),
    }
}

/// Tipo pela aparência (LE): categoria nos 10 bits altos, subcategoria nos 6 baixos.
fn le_glyph(raw: u16) -> u16 {
    match (raw >> 6, raw & 0x3F) {
        (1, _) => G_PHONE,
        (15, 1) => G_KEYBOARD,
        (15, 2) => G_MOUSE,
        (33 | 34 | 37, _) => G_HEADPHONE,
        _ => glyph::BLUETOOTH,
    }
}

fn find_all(selector: windows::core::Result<HSTRING>) -> Vec<DeviceInformation> {
    selector
        .and_then(|s| DeviceInformation::FindAllAsyncAqsFilter(&s))
        .and_then(|op| op.join())
        .map(|c| c.into_iter().collect())
        .unwrap_or_default()
}

/// Pareados: clássicos e LE (sem repetir o mesmo aparelho), conectados primeiro.
fn load() -> Vec<Device> {
    let mut out: Vec<Device> = Vec::new();
    for info in find_all(BluetoothDevice::GetDeviceSelectorFromPairingState(true)) {
        let Ok(id) = info.Id() else { continue };
        let Ok(d) = BluetoothDevice::FromIdAsync(&id).and_then(|op| op.join()) else { continue };
        let name = d.Name().or_else(|_| info.Name()).map(|n| n.to_string()).unwrap_or_default();
        let (glyph, audio) = classic_glyph(d.ClassOfDevice().and_then(|c| c.RawValue()).unwrap_or(0));
        out.push(Device {
            id,
            name,
            glyph,
            le: false,
            audio,
            address: d.BluetoothAddress().unwrap_or(0),
            connected: d.ConnectionStatus() == Ok(BluetoothConnectionStatus::Connected),
            busy: false,
            unsupported: false,
        });
    }
    for info in find_all(BluetoothLEDevice::GetDeviceSelectorFromPairingState(true)) {
        let Ok(id) = info.Id() else { continue };
        let Ok(d) = BluetoothLEDevice::FromIdAsync(&id).and_then(|op| op.join()) else { continue };
        let name = d.Name().or_else(|_| info.Name()).map(|n| n.to_string()).unwrap_or_default();
        let address = d.BluetoothAddress().unwrap_or(0);
        if out.iter().any(|o| o.address == address || (!name.is_empty() && o.name == name)) {
            continue;
        }
        out.push(Device {
            id,
            name,
            glyph: le_glyph(d.Appearance().and_then(|a| a.RawValue()).unwrap_or(0)),
            le: true,
            audio: false,
            address,
            connected: d.ConnectionStatus() == Ok(BluetoothConnectionStatus::Connected),
            busy: false,
            unsupported: false,
        });
    }
    for d in &mut out {
        if d.name.is_empty() {
            d.name = "Dispositivo Bluetooth".into();
        }
    }
    out.sort_by(|a, b| b.connected.cmp(&a.connected).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out.truncate(MAX_DEVICES);
    out
}

/// Conecta (desliga e religa os serviços, o que força a reconexão) ou desconecta (desliga
/// os serviços) um dispositivo clássico. `false` = nenhum serviço aceitou a troca.
fn switch_services(address: u64, audio: bool, connect: bool) -> bool {
    unsafe {
        let params = BLUETOOTH_FIND_RADIO_PARAMS { dwSize: size_of::<BLUETOOTH_FIND_RADIO_PARAMS>() as u32 };
        let mut radio = HANDLE::default();
        let Ok(find) = BluetoothFindFirstRadio(&params, &mut radio) else { return false };
        let _ = BluetoothFindRadioClose(find);

        let mut info = BLUETOOTH_DEVICE_INFO { dwSize: size_of::<BLUETOOTH_DEVICE_INFO>() as u32, ..Default::default() };
        info.Address.Anonymous.ullLong = address;
        let mut any = false;
        if BluetoothGetDeviceInfo(Some(radio), &mut info) == 0 {
            let mut guids = [GUID::zeroed(); 32];
            let mut n = guids.len() as u32;
            let installed: &[GUID] =
                if BluetoothEnumerateInstalledServices(Some(radio), &info, &mut n, Some(guids.as_mut_ptr())) == 0 {
                    &guids[..(n as usize).min(guids.len())]
                } else {
                    &[]
                };
            let mut services: Vec<GUID> = installed.iter().filter(|g| KNOWN.contains(g)).copied().collect();
            // Serviços desligados (ex.: desconectado por aqui) não aparecem como instalados.
            if services.is_empty() && connect {
                services = if audio { vec![A2DP_SINK, HANDSFREE, AVRCP] } else { vec![HID] };
            }
            for g in &services {
                if connect {
                    let _ = BluetoothSetServiceState(Some(radio), &info, g, BLUETOOTH_SERVICE_DISABLE);
                }
                let flag = if connect { BLUETOOTH_SERVICE_ENABLE } else { BLUETOOTH_SERVICE_DISABLE };
                any |= BluetoothSetServiceState(Some(radio), &info, g, flag) == 0;
            }
        }
        let _ = CloseHandle(radio);
        any
    }
}

/// Resultados das threads, lidos pelo painel no `notify`.
#[derive(Default)]
struct Shared {
    list: Option<Vec<Device>>,
    /// Dispositivo, conectado agora (None = não suportado).
    updates: Vec<(HSTRING, Option<bool>)>,
}

const TOGGLE: u32 = 1;
const SETTINGS: u32 = 2;
/// Linhas de dispositivo: DEVICE + índice.
const DEVICE: u32 = 100;

pub struct BluetoothPanel {
    on: bool,
    /// None = carregando.
    devices: Option<Vec<Device>>,
    shared: Arc<Mutex<Shared>>,
}

impl BluetoothPanel {
    pub fn new() -> Self {
        let mut p = Self { on: radio().is_some_and(|r| radio_on(&r)), devices: None, shared: Arc::default() };
        p.reload();
        p
    }

    fn reload(&mut self) {
        self.devices = None;
        let shared = self.shared.clone();
        spawn(move || {
            let list = load();
            if let Ok(mut s) = shared.lock() {
                s.list = Some(list);
            }
            post(WM_BT_PANEL, 0);
        });
    }

    /// Alterna a conexão do dispositivo `i` numa thread; o resultado volta por WM_BT_PANEL.
    fn switch(&mut self, i: usize) {
        let Some(d) = self.devices.as_mut().and_then(|l| l.get_mut(i)) else { return };
        if d.busy {
            return;
        }
        if d.le {
            d.unsupported = true;
            return;
        }
        d.busy = true;
        d.unsupported = false;
        let (id, address, audio, connect) = (d.id.clone(), d.address, d.audio, !d.connected);
        let shared = self.shared.clone();
        spawn(move || {
            let result = if switch_services(address, audio, connect) {
                // Espera a conexão mudar (até ~10 s) e lê o estado final.
                let mut now = !connect;
                for _ in 0..20 {
                    std::thread::sleep(Duration::from_millis(500));
                    let Ok(d) = BluetoothDevice::FromIdAsync(&id).and_then(|op| op.join()) else { break };
                    now = d.ConnectionStatus() == Ok(BluetoothConnectionStatus::Connected);
                    if now == connect {
                        break;
                    }
                }
                Some(now)
            } else {
                None
            };
            if let Ok(mut s) = shared.lock() {
                s.updates.push((id, result));
            }
            post(WM_BT_PANEL, 0);
        });
    }
}

impl PanelContent for BluetoothPanel {
    fn build(&mut self) -> Vec<Widget> {
        let mut out = vec![
            Widget::Title("Bluetooth".into()),
            Widget::Toggle { id: TOGGLE, text: if self.on { "Ligado" } else { "Desligado" }.into(), on: self.on, icon: None },
            Widget::Separator,
        ];
        if !self.on {
            out.push(Widget::Text("O Bluetooth está desligado.".into()));
        } else {
            out.push(Widget::Heading { text: "Dispositivos".into(), buttons: Vec::new() });
            match &self.devices {
                None => out.push(Widget::Text("Carregando…".into())),
                Some(l) if l.is_empty() => out.push(Widget::Text("Nenhum dispositivo pareado.".into())),
                Some(l) => {
                    for (i, d) in l.iter().enumerate() {
                        let sub = match d {
                            Device { busy: true, connected: true, .. } => "Desconectando…",
                            Device { busy: true, .. } => "Conectando…",
                            Device { unsupported: true, .. } => "Não suportado",
                            Device { connected: true, .. } => "Conectado",
                            _ => "Não conectado",
                        };
                        out.push(Widget::Row {
                            id: DEVICE + i as u32,
                            glyph: Some(d.glyph),
                            text: d.name.clone(),
                            sub: Some(sub.into()),
                            selected: false,
                        });
                    }
                }
            }
        }
        out.push(Widget::Separator);
        out.push(Widget::Row {
            id: SETTINGS,
            glyph: Some(glyph::SETTINGS),
            text: "Configurações de Bluetooth".into(),
            sub: None,
            selected: false,
        });
        out
    }

    fn event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Toggle(TOGGLE, on) => {
                self.on = on;
                set_radio(on);
                if on {
                    self.reload();
                }
                true
            }
            Event::Click(SETTINGS) => {
                unsafe {
                    ShellExecuteW(None, w!("open"), w!("ms-settings:bluetooth"), None, None, SW_SHOWNORMAL);
                }
                panel::close();
                false
            }
            Event::Click(id) if id >= DEVICE => {
                self.switch((id - DEVICE) as usize);
                true
            }
            _ => false,
        }
    }

    fn notify(&mut self, msg: u32, wparam: usize) -> bool {
        match msg {
            WM_BT_RADIO => {
                let on = wparam == 2;
                if on == self.on {
                    return false;
                }
                self.on = on;
                if on {
                    self.reload();
                }
                true
            }
            WM_BT_PANEL => {
                let Ok(mut s) = self.shared.lock() else { return false };
                if let Some(list) = s.list.take() {
                    self.devices = Some(list);
                }
                let updates = std::mem::take(&mut s.updates);
                drop(s);
                if let Some(list) = &mut self.devices {
                    for (id, result) in updates {
                        if let Some(d) = list.iter_mut().find(|d| d.id == id) {
                            d.busy = false;
                            match result {
                                Some(c) => d.connected = c,
                                None => d.unsupported = true,
                            }
                        }
                    }
                }
                true
            }
            _ => false,
        }
    }
}
