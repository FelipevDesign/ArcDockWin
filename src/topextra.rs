//! Recursos extras da barra do topo: menu do sistema, app em foco, áreas de trabalho
//! virtuais, captura de tela, clima, próximo compromisso, bateria, notificações, Não
//! incomodar e relógios mundiais.
//!
//! O que vem da internet (clima e agenda) é lido numa thread e avisa a barra com
//! [`WM_EXTRA`]; o resto é lido na hora, só com o módulo ligado.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{DISPLAY_DEVICEW, EnumDisplayDevicesW};
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::Win32::System::SystemInformation::{GetLocalTime, GetPhysicallyInstalledSystemMemory};
use windows::Win32::System::Threading::{GetCurrentProcessId, OpenProcess, PROCESS_TERMINATE, TerminateProcess};
use windows::Win32::System::Time::{
    DYNAMIC_TIME_ZONE_INFORMATION, EnumDynamicTimeZoneInformation, SystemTimeToTzSpecificLocalTime,
    SystemTimeToTzSpecificLocalTimeEx,
};
use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, SendInput,
    VIRTUAL_KEY, VK_CONTROL, VK_LEFT, VK_LWIN, VK_N, VK_RIGHT, VK_S, VK_SHIFT,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetClassNameW, GetForegroundWindow, GetWindow, GetWindowLongW,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsHungAppWindow, IsWindowVisible, PostMessageW,
    SW_MINIMIZE, SW_SHOWNORMAL, ShowWindow, WM_APP, WM_CLOSE, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, GUID, HSTRING, PCWSTR, w};

use crate::config;
use crate::extras;
use crate::i18n::tr;
use crate::panel::{self, Event, PanelContent, Widget, glyph};

/// Algo chegou de uma thread (clima, agenda, notificações): a barra refaz os itens.
pub const WM_EXTRA: u32 = WM_APP + 75;

// ---------------------------------------------------------------------------------------------
// Utilidades

fn wide(s: &str) -> HSTRING {
    HSTRING::from(s)
}

fn reg_raw(root: HKEY, key: &str, name: &str, kind: windows::Win32::System::Registry::REG_ROUTINE_FLAGS) -> Option<Vec<u8>> {
    unsafe {
        let (k, n) = (wide(key), wide(name));
        let mut len = 0u32;
        RegGetValueW(root, &k, &n, kind, None, None, Some(&mut len)).ok().ok()?;
        let mut buf = vec![0u8; len as usize];
        RegGetValueW(root, &k, &n, kind, None, Some(buf.as_mut_ptr() as _), Some(&mut len)).ok().ok()?;
        buf.truncate(len as usize);
        Some(buf)
    }
}

fn reg_str(root: HKEY, key: &str, name: &str) -> Option<String> {
    let b = reg_raw(root, key, name, RRF_RT_REG_SZ)?;
    let w: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
    let s = String::from_utf16_lossy(&w).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn reg_u32(root: HKEY, key: &str, name: &str) -> Option<u32> {
    let b = reg_raw(root, key, name, RRF_RT_REG_DWORD)?;
    Some(u32::from_le_bytes(b.get(..4)?.try_into().ok()?))
}

/// Abre um endereço, arquivo ou app pelo shell.
pub fn open(target: &str) {
    unsafe {
        ShellExecuteW(None, w!("open"), &wide(target), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// Aperta as teclas em ordem e solta na ordem inversa.
fn keys(list: &[VIRTUAL_KEY]) {
    let ext = |k: VIRTUAL_KEY| if [VK_LEFT, VK_RIGHT, VK_LWIN].contains(&k) { KEYEVENTF_EXTENDEDKEY } else { KEYBD_EVENT_FLAGS(0) };
    let ev = |k: VIRTUAL_KEY, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: k, dwFlags: if up { ext(k) | KEYEVENTF_KEYUP } else { ext(k) }, ..Default::default() },
        },
    };
    let mut inputs: Vec<INPUT> = list.iter().map(|&k| ev(k, false)).collect();
    inputs.extend(list.iter().rev().map(|&k| ev(k, true)));
    unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
}

fn class_name(h: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(h, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n])
}

/// Janela de topo "de verdade" (a que aparece no Alt+Tab).
fn real_window(h: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(h).as_bool() || GetWindow(h, GW_OWNER).is_ok_and(|o| !o.is_invalid()) {
            return false;
        }
        if GetWindowLongW(h, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 || GetWindowTextLengthW(h) == 0 {
            return false;
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut u32 as _, 4);
        cloaked == 0
    }
}

fn windows_of(pid: Option<u32>) -> Vec<HWND> {
    unsafe extern "system" fn each(h: HWND, lp: LPARAM) -> BOOL {
        let list = unsafe { &mut *(lp.0 as *mut Vec<HWND>) };
        list.push(h);
        BOOL(1)
    }
    let mut all: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut all as *mut _ as isize));
    }
    all.into_iter()
        .filter(|&h| real_window(h))
        .filter(|&h| {
            pid.is_none_or(|p| {
                let mut q = 0u32;
                unsafe { GetWindowThreadProcessId(h, Some(&mut q)) };
                q == p
            })
        })
        .collect()
}

fn post_bar(what: usize) {
    let h = crate::topbar::hwnd();
    if !h.is_invalid() {
        unsafe {
            let _ = PostMessageW(Some(h), WM_EXTRA, WPARAM(what), LPARAM(0));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Datas (minutos desde 1970-01-01, hora local)

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { yoe + era * 400 + 1 } else { yoe + era * 400 }, m, d)
}

/// Dia da semana (0 = domingo) de um dia contado desde 1970-01-01 (uma quinta).
fn weekday_of(day: i64) -> usize {
    (day + 4).rem_euclid(7) as usize
}

fn minutes_of(t: &SYSTEMTIME) -> i64 {
    days_from_civil(t.wYear as i64, t.wMonth as u32, t.wDay as u32) * 1440 + t.wHour as i64 * 60 + t.wMinute as i64
}

fn now_minutes() -> i64 {
    minutes_of(&unsafe { GetLocalTime() })
}

fn hhmm(m: i64) -> String {
    let m = m.rem_euclid(1440);
    if config::clock_24h() {
        format!("{:02}:{:02}", m / 60, m % 60)
    } else {
        let h = (m / 60) % 12;
        format!("{}:{:02} {}", if h == 0 { 12 } else { h }, m % 60, if m < 720 { "AM" } else { "PM" })
    }
}

const SHORT_DAYS: [&str; 7] = ["dom", "seg", "ter", "qua", "qui", "sex", "sáb"];

// ---------------------------------------------------------------------------------------------
// App em foco

#[derive(Clone, Default, PartialEq)]
pub struct FocusApp {
    pub hwnd: isize,
    pub pid: u32,
    pub source: String,
    pub name: String,
}

thread_local! {
    static FOCUS: RefCell<Option<FocusApp>> = const { RefCell::new(None) };
    static RECENT: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    static RECENT_SAVED: Cell<Option<Instant>> = const { Cell::new(None) };
}

pub fn focus() -> Option<FocusApp> {
    FOCUS.with_borrow(|f| f.clone())
}

/// Relê o app em primeiro plano; `true` se mudou o nome mostrado. Janelas do próprio dock
/// (painéis, Configurações) não contam: fica o app de antes.
pub fn update_focus() -> bool {
    let h = unsafe { GetForegroundWindow() };
    if h.is_invalid() {
        return false;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    if pid == unsafe { GetCurrentProcessId() } {
        return false;
    }
    let class = class_name(h);
    let next = if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
        FocusApp { hwnd: 0, pid, source: String::new(), name: tr("Área de trabalho") }
    } else if matches!(class.as_str(), "ForegroundStaging" | "XamlExplorerHostIslandWindow" | "MultitaskingViewFrame") {
        return false;
    } else {
        let Some((_, source)) = crate::apps::window_app(h, &mut HashMap::new()) else { return false };
        let name = crate::apps::display_name(&source);
        FocusApp { hwnd: h.0 as isize, pid, source, name }
    };
    if !next.source.is_empty() {
        remember_app(&next.source);
    }
    FOCUS.with_borrow_mut(|f| {
        let changed = f.as_ref().is_none_or(|o| o.name != next.name || o.source != next.source);
        *f = Some(next);
        changed
    })
}

/// Apps usados por último (mais recente primeiro), guardados nas Configurações.
fn recent_apps() -> Vec<String> {
    RECENT.with_borrow_mut(|r| {
        r.get_or_insert_with(|| {
            config::raw("recent_apps").map(|v| v.split('|').filter(|s| !s.is_empty()).map(String::from).collect()).unwrap_or_default()
        })
        .clone()
    })
}

fn remember_app(source: &str) {
    let mut list = recent_apps();
    if list.first().is_some_and(|s| s == source) {
        return;
    }
    list.retain(|s| s != source);
    list.insert(0, source.to_string());
    list.truncate(8);
    let save = list.join("|");
    RECENT.with_borrow_mut(|r| *r = Some(list));
    // Grava no máximo a cada 30 s (trocar de app é frequente).
    if RECENT_SAVED.get().is_none_or(|t| t.elapsed() > Duration::from_secs(30)) {
        RECENT_SAVED.set(Some(Instant::now()));
        config::set_raw("recent_apps", save);
    }
}

/// Arquivos abertos por último (atalhos da pasta Recentes do Windows).
fn recent_files(n: usize) -> Vec<(String, std::path::PathBuf)> {
    let Some(dir) = std::env::var_os("APPDATA").map(|a| std::path::Path::new(&a).join("Microsoft\\Windows\\Recent")) else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut list: Vec<(std::time::SystemTime, String, std::path::PathBuf)> = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")))
        .filter_map(|e| {
            let t = e.metadata().ok()?.modified().ok()?;
            let name = e.path().file_stem()?.to_string_lossy().into_owned();
            Some((t, name, e.path()))
        })
        .collect();
    list.sort_by(|a, b| b.0.cmp(&a.0));
    list.into_iter().take(n).map(|(_, n, p)| (n, p)).collect()
}

const APP_NEW: u32 = 1;
const APP_MIN: u32 = 2;
const APP_CLOSE: u32 = 3;
const APP_SHOW: u32 = 4;
const APP_QUIT: u32 = 5;
const APP_FORCE: u32 = 6;

/// Menu do app em foco (clique no nome dele).
pub struct AppMenu {
    app: Option<FocusApp>,
}

impl AppMenu {
    pub fn new() -> Self {
        Self { app: focus() }
    }
}

impl PanelContent for AppMenu {
    fn width(&self) -> f32 {
        250.0
    }
    fn left_aligned(&self) -> bool {
        true
    }
    fn build(&mut self) -> Vec<Widget> {
        let row = |id, g: u16, t: &str| Widget::Row { id, glyph: Some(g), text: t.into(), sub: None, selected: false };
        let Some(app) = &self.app else { return vec![Widget::Text(tr("Nenhum app em foco"))] };
        let mut out = vec![Widget::Heading { text: app.name.clone(), buttons: Vec::new() }];
        if app.source.is_empty() {
            out.push(row(APP_NEW, 0xEC50, "Novo Explorador de Arquivos"));
            return out;
        }
        out.push(row(APP_NEW, 0xE710, "Nova janela"));
        if app.hwnd != 0 {
            out.push(row(APP_MIN, 0xE921, "Minimizar"));
            out.push(row(APP_CLOSE, 0xE8BB, "Fechar janela"));
        }
        if std::path::Path::new(&app.source).is_file() {
            out.push(row(APP_SHOW, 0xEC50, "Mostrar no Explorador"));
        }
        out.push(Widget::Separator);
        out.push(row(APP_QUIT, 0xE7E8, &format!("{} {}", tr("Encerrar"), app.name)));
        out.push(row(APP_FORCE, 0xE7BA, "Forçar encerramento…"));
        out
    }
    fn event(&mut self, ev: Event) -> bool {
        let Event::Click(id) = ev else { return false };
        let Some(app) = self.app.clone() else { return false };
        let hwnd = HWND(app.hwnd as _);
        panel::close();
        unsafe {
            match id {
                APP_NEW if app.source.is_empty() => open("explorer.exe"),
                APP_NEW => crate::apps::launch(&app.source, ""),
                APP_MIN => {
                    let _ = ShowWindow(hwnd, SW_MINIMIZE);
                }
                APP_CLOSE => {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                APP_SHOW => {
                    let args = wide(&format!("/select,\"{}\"", app.source));
                    ShellExecuteW(None, w!("open"), w!("explorer.exe"), &args, PCWSTR::null(), SW_SHOWNORMAL);
                }
                // Como o "Encerrar" do macOS: pede para fechar todas as janelas do app.
                APP_QUIT => {
                    for h in windows_of(Some(app.pid)) {
                        let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                }
                APP_FORCE => {
                    let r = panel_anchor();
                    panel::toggle("sysmenu", r, || Box::new(SysMenu::at(View::Force)));
                }
                _ => {}
            }
        }
        false
    }
}

/// Retângulo do item do menu do sistema (ou o topo da tela), para abrir painéis.
fn panel_anchor() -> windows::Win32::Foundation::RECT {
    crate::topbar::item_screen_rect(crate::config::Module::Menu)
        .or_else(|| crate::topbar::item_screen_rect(crate::config::Module::App))
        .unwrap_or(windows::Win32::Foundation::RECT { left: 8, top: 0, right: 40, bottom: 24 })
}

// ---------------------------------------------------------------------------------------------
// Áreas de trabalho virtuais

const VD_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\VirtualDesktops";

fn guid_text(b: &[u8]) -> String {
    let d1 = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let d2 = u16::from_le_bytes([b[4], b[5]]);
    let d3 = u16::from_le_bytes([b[6], b[7]]);
    format!(
        "{{{d1:08X}-{d2:04X}-{d3:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

/// Nomes das áreas de trabalho e a atual; None com uma só (o Windows nem guarda a lista).
pub fn desktops() -> Option<(Vec<String>, usize)> {
    let ids = reg_raw(HKEY_CURRENT_USER, VD_KEY, "VirtualDesktopIDs", RRF_RT_REG_BINARY)?;
    let cur = reg_raw(HKEY_CURRENT_USER, VD_KEY, "CurrentVirtualDesktop", RRF_RT_REG_BINARY);
    let list: Vec<&[u8]> = ids.chunks_exact(16).collect();
    if list.len() < 2 {
        return None;
    }
    let names = list
        .iter()
        .enumerate()
        .map(|(i, id)| {
            reg_str(HKEY_CURRENT_USER, &format!("{VD_KEY}\\Desktops\\{}", guid_text(id)), "Name")
                .unwrap_or_else(|| format!("{} {}", tr("Área de trabalho"), i + 1))
        })
        .collect();
    let current = cur.and_then(|c| list.iter().position(|id| *id == &c[..])).unwrap_or(0);
    Some((names, current))
}

/// Vai `delta` áreas de trabalho para a direita (negativo: esquerda), uma de cada vez.
pub fn switch_desktop(delta: i32) {
    if delta == 0 {
        return;
    }
    std::thread::spawn(move || {
        let arrow = if delta > 0 { VK_RIGHT } else { VK_LEFT };
        for i in 0..delta.unsigned_abs() {
            if i > 0 {
                std::thread::sleep(Duration::from_millis(180));
            }
            keys(&[VK_CONTROL, VK_LWIN, arrow]);
        }
    });
}

pub struct DesktopPanel;

impl PanelContent for DesktopPanel {
    fn width(&self) -> f32 {
        240.0
    }
    fn left_aligned(&self) -> bool {
        true
    }
    fn build(&mut self) -> Vec<Widget> {
        let Some((names, cur)) = desktops() else { return vec![Widget::Text(tr("Só uma área de trabalho"))] };
        let mut out: Vec<Widget> = names
            .into_iter()
            .enumerate()
            .map(|(i, n)| Widget::Row { id: i as u32 + 1, glyph: Some(0xE7F4), text: n, sub: None, selected: i == cur })
            .collect();
        out.push(Widget::Separator);
        out.push(Widget::Row { id: 1000, glyph: Some(0xE7C4), text: tr("Visão de tarefas"), sub: None, selected: false });
        out
    }
    fn event(&mut self, ev: Event) -> bool {
        let Event::Click(id) = ev else { return false };
        panel::close();
        if id == 1000 {
            keys(&[VK_LWIN, VIRTUAL_KEY(0x09)]);
        } else if let Some((_, cur)) = desktops() {
            switch_desktop(id as i32 - 1 - cur as i32);
        }
        false
    }
}

// ---------------------------------------------------------------------------------------------
// Menu do sistema

#[derive(Clone, Copy, PartialEq)]
pub enum View {
    Main,
    About,
    Recent,
    Force,
    Confirm(u32, u32),
}

const M_ABOUT: u32 = 1;
const M_SETTINGS: u32 = 2;
const M_STORE: u32 = 3;
const M_RECENT: u32 = 4;
const M_FORCE: u32 = 5;
const M_SLEEP: u32 = 6;
const M_RESTART: u32 = 7;
const M_SHUTDOWN: u32 = 8;
const M_LOCK: u32 = 9;
const M_SIGNOUT: u32 = 10;
const M_BACK: u32 = 11;
const M_MORE: u32 = 12;
const M_OK: u32 = 13;
const M_CANCEL: u32 = 14;
/// Apps recentes: 100 + índice; arquivos: 200 + índice; processos: 1000 + pid.
const M_APP: u32 = 100;
const M_FILE: u32 = 200;
const M_KILL: u32 = 1000;

pub struct SysMenu {
    view: View,
    files: Vec<(String, std::path::PathBuf)>,
    apps: Vec<String>,
    procs: Vec<(u32, String, bool)>,
}

impl SysMenu {
    pub fn new() -> Self {
        Self::at(View::Main)
    }
    pub fn at(view: View) -> Self {
        let mut m = Self { view: View::Main, files: Vec::new(), apps: Vec::new(), procs: Vec::new() };
        m.go(view);
        m
    }
    fn go(&mut self, view: View) {
        match view {
            View::Recent => {
                self.apps = recent_apps();
                self.files = recent_files(8);
            }
            View::Force => self.procs = running_apps(),
            _ => {}
        }
        self.view = view;
    }
}

/// Apps com janela, um por processo; os que não respondem primeiro.
fn running_apps() -> Vec<(u32, String, bool)> {
    let own = unsafe { GetCurrentProcessId() };
    let mut seen: HashMap<u32, (String, bool)> = HashMap::new();
    for h in windows_of(None) {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
        if pid == own || pid == 0 {
            continue;
        }
        let hung = unsafe { IsHungAppWindow(h) }.as_bool();
        let entry = seen.entry(pid).or_insert_with(|| {
            let name = crate::apps::window_app(h, &mut HashMap::new())
                .map(|(_, s)| crate::apps::display_name(&s))
                .unwrap_or_else(|| {
                    let mut t = [0u16; 128];
                    let n = unsafe { GetWindowTextW(h, &mut t) } as usize;
                    String::from_utf16_lossy(&t[..n])
                });
            (name, false)
        });
        entry.1 |= hung;
    }
    let mut list: Vec<(u32, String, bool)> = seen.into_iter().map(|(p, (n, h))| (p, n, h)).collect();
    list.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
    list
}

/// Linhas de "Sobre este PC": (glifo, rótulo, valor).
fn about() -> (String, Vec<(u16, &'static str, String)>) {
    let bios = "HARDWARE\\DESCRIPTION\\System\\BIOS";
    let maker = reg_str(HKEY_LOCAL_MACHINE, bios, "SystemManufacturer").unwrap_or_default();
    let product = reg_str(HKEY_LOCAL_MACHINE, bios, "SystemProductName").unwrap_or_default();
    let model = if product.to_lowercase().contains(&maker.to_lowercase()) || maker.is_empty() {
        product
    } else {
        format!("{maker} {product}")
    };
    let cpu = reg_str(HKEY_LOCAL_MACHINE, "HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0", "ProcessorNameString")
        .unwrap_or_else(|| "—".into());
    let mut kb = 0u64;
    let ram = if unsafe { GetPhysicallyInstalledSystemMemory(&mut kb) }.is_ok() {
        format!("{} GB", (kb as f64 / 1024.0 / 1024.0).round())
    } else {
        "—".into()
    };
    let gpu = {
        let mut names: Vec<String> = Vec::new();
        for i in 0..8 {
            let mut d = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
            if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), i, &mut d, 0) }.as_bool() {
                break;
            }
            let n = d.DeviceString.iter().position(|&c| c == 0).unwrap_or(d.DeviceString.len());
            let s = String::from_utf16_lossy(&d.DeviceString[..n]);
            if !s.is_empty() && !names.contains(&s) {
                names.push(s);
            }
        }
        if names.is_empty() { "—".into() } else { names.join(" · ") }
    };
    let disk = {
        let (mut free, mut total) = (0u64, 0u64);
        if unsafe { GetDiskFreeSpaceExW(w!("C:\\"), None, Some(&mut total), Some(&mut free)) }.is_ok() {
            let gb = |b: u64| (b as f64 / 1e9).round();
            format!("{} GB {} {} GB", gb(free), tr("livres de"), gb(total))
        } else {
            "—".into()
        }
    };
    let nt = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion";
    let build: u32 = reg_str(HKEY_LOCAL_MACHINE, nt, "CurrentBuild").and_then(|b| b.parse().ok()).unwrap_or(0);
    let mut product = reg_str(HKEY_LOCAL_MACHINE, nt, "ProductName").unwrap_or_else(|| "Windows".into());
    if build >= 22000 {
        product = product.replace("Windows 10", "Windows 11");
    }
    let ver = reg_str(HKEY_LOCAL_MACHINE, nt, "DisplayVersion").unwrap_or_default();
    let ubr = reg_u32(HKEY_LOCAL_MACHINE, nt, "UBR").map(|u| format!(".{u}")).unwrap_or_default();
    let windows = format!("{product} {ver} ({} {build}{ubr})", tr("compilação"));
    let model = if model.trim().is_empty() { tr("Este PC") } else { model };
    (
        model,
        vec![
            (0xE950, "Processador", cpu),
            (0xE964, "Memória", ram),
            (0xE7F4, "Placa de vídeo", gpu),
            (0xEDA2, "Disco (C:)", disk),
            (0xE782, "Windows", windows),
        ],
    )
}

impl PanelContent for SysMenu {
    fn width(&self) -> f32 {
        if self.view == View::About { 340.0 } else { 260.0 }
    }
    fn left_aligned(&self) -> bool {
        true
    }
    fn build(&mut self) -> Vec<Widget> {
        let row = |id, g: u16, t: &str| Widget::Row { id, glyph: Some(g), text: t.into(), sub: None, selected: false };
        let back = || Widget::Row { id: M_BACK, glyph: Some(glyph::CHEVRON_LEFT), text: tr("Menu"), sub: None, selected: false };
        match self.view {
            View::Main => vec![
                row(M_ABOUT, 0xE946, "Sobre este PC"),
                Widget::Separator,
                row(M_SETTINGS, glyph::SETTINGS, "Configurações do Windows"),
                row(M_STORE, 0xE719, "Microsoft Store"),
                Widget::Separator,
                row(M_RECENT, 0xE81C, "Itens recentes"),
                Widget::Separator,
                row(M_FORCE, 0xE7BA, "Forçar encerramento…"),
                Widget::Separator,
                row(M_SLEEP, 0xE708, "Suspender"),
                row(M_RESTART, 0xE777, "Reiniciar…"),
                row(M_SHUTDOWN, 0xE7E8, "Desligar…"),
                Widget::Separator,
                row(M_LOCK, glyph::LOCK, "Bloquear tela"),
                row(M_SIGNOUT, 0xF3B1, "Sair da conta"),
            ],
            View::About => {
                let (model, lines) = about();
                let mut out = vec![back(), Widget::Banner { glyph: 0xE7F8, image: None, text: model, sub: computer_name() }];
                out.push(Widget::Separator);
                for (g, label, value) in lines {
                    out.push(Widget::Row { id: 0, glyph: Some(g), text: tr(label), sub: Some(value), selected: false });
                }
                out.push(Widget::Separator);
                out.push(Widget::Button { id: M_MORE, text: tr("Mais informações"), primary: false });
                out
            }
            View::Recent => {
                let mut out = vec![back()];
                if !self.apps.is_empty() {
                    out.push(Widget::Label(tr("Apps")));
                    for (i, s) in self.apps.iter().enumerate() {
                        out.push(row(M_APP + i as u32, 0xE737, &crate::apps::display_name(s)));
                    }
                }
                if !self.files.is_empty() {
                    out.push(Widget::Label(tr("Arquivos")));
                    for (i, (n, _)) in self.files.iter().enumerate() {
                        out.push(row(M_FILE + i as u32, 0xE8A5, n));
                    }
                }
                if self.apps.is_empty() && self.files.is_empty() {
                    out.push(Widget::Text(tr("Nada por aqui ainda")));
                }
                out
            }
            View::Force => {
                let mut out = vec![back(), Widget::Text(tr("Escolha o app. O que não foi salvo nele será perdido."))];
                for (pid, name, hung) in &self.procs {
                    let sub = hung.then(|| tr("Não está respondendo"));
                    if *hung {
                        out.push(Widget::RowBadge {
                            id: M_KILL + pid,
                            glyph: Some(0xE7BA),
                            text: name.clone(),
                            sub,
                            selected: true,
                            badge: 0xE783,
                        });
                    } else {
                        out.push(Widget::Row { id: M_KILL + pid, glyph: Some(0xE737), text: name.clone(), sub, selected: false });
                    }
                }
                out
            }
            View::Confirm(what, pid) => {
                let (text, button) = match what {
                    M_RESTART => (tr("Reiniciar o computador agora?"), tr("Reiniciar")),
                    M_SHUTDOWN => (tr("Desligar o computador agora?"), tr("Desligar")),
                    _ => {
                        let name = self.procs.iter().find(|p| p.0 == pid).map(|p| p.1.clone()).unwrap_or_default();
                        (format!("{} {name}? {}", tr("Forçar o encerramento de"), tr("O que não foi salvo será perdido.")), tr("Forçar encerramento"))
                    }
                };
                vec![
                    Widget::Text(text),
                    Widget::Button { id: M_OK, text: button, primary: true },
                    Widget::Button { id: M_CANCEL, text: tr("Cancelar"), primary: false },
                ]
            }
        }
    }
    fn event(&mut self, ev: Event) -> bool {
        let Event::Click(id) = ev else { return false };
        let close_then = |f: &dyn Fn()| {
            panel::close();
            f();
            false
        };
        match (self.view, id) {
            (_, M_BACK) | (View::Confirm(..), M_CANCEL) => {
                let back = match self.view {
                    View::Confirm(_, pid) if pid != 0 => View::Force,
                    _ => View::Main,
                };
                self.go(back);
                true
            }
            (View::Main, M_ABOUT) => {
                self.go(View::About);
                true
            }
            (View::Main, M_RECENT) => {
                self.go(View::Recent);
                true
            }
            (View::Main, M_FORCE) => {
                self.go(View::Force);
                true
            }
            (View::Main, M_RESTART | M_SHUTDOWN) => {
                self.go(View::Confirm(id, 0));
                true
            }
            (View::Main, M_SETTINGS) => close_then(&|| open("ms-settings:")),
            (View::Main, M_STORE) => close_then(&|| open("ms-windows-store:")),
            (View::Main, M_SLEEP) => close_then(&|| crate::power::run(crate::power::SLEEP)),
            (View::Main, M_LOCK) => close_then(&|| crate::power::run(crate::power::LOCK)),
            (View::Main, M_SIGNOUT) => close_then(&|| crate::power::run(crate::power::SIGN_OUT)),
            (View::About, M_MORE) => close_then(&|| open("ms-settings:about")),
            (View::Recent, id) if (M_APP..M_FILE).contains(&id) => {
                if let Some(s) = self.apps.get((id - M_APP) as usize).cloned() {
                    panel::close();
                    crate::apps::launch(&s, "");
                }
                false
            }
            (View::Recent, id) if (M_FILE..M_KILL).contains(&id) => {
                if let Some((_, p)) = self.files.get((id - M_FILE) as usize).cloned() {
                    panel::close();
                    open(&p.to_string_lossy());
                }
                false
            }
            (View::Force, id) if id >= M_KILL => {
                self.view = View::Confirm(M_KILL, id - M_KILL);
                true
            }
            (View::Confirm(what, pid), M_OK) => {
                panel::close();
                match what {
                    M_RESTART => crate::power::run(crate::power::RESTART),
                    M_SHUTDOWN => crate::power::run(crate::power::SHUTDOWN),
                    _ => unsafe {
                        if let Ok(p) = OpenProcess(PROCESS_TERMINATE, false, pid) {
                            let _ = TerminateProcess(p, 1);
                            let _ = CloseHandle(p);
                        }
                    },
                }
                false
            }
            _ => false,
        }
    }
}

fn computer_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// Captura de tela

const SHOT_AREA: u32 = 1;
const SHOT_WINDOW: u32 = 2;
const SHOT_FULL: u32 = 3;
const SHOT_VIDEO: u32 = 4;
const SHOT_APP: u32 = 5;

pub struct ShotPanel;

impl PanelContent for ShotPanel {
    fn width(&self) -> f32 {
        230.0
    }
    fn build(&mut self) -> Vec<Widget> {
        let row = |id, g: u16, t: &str| Widget::Row { id, glyph: Some(g), text: t.into(), sub: None, selected: false };
        vec![
            row(SHOT_AREA, 0xE7A8, "Capturar área"),
            row(SHOT_WINDOW, 0xE737, "Capturar janela"),
            row(SHOT_FULL, 0xE740, "Capturar tela inteira"),
            row(SHOT_VIDEO, 0xE714, "Gravar a tela"),
            Widget::Separator,
            row(SHOT_APP, 0xE70F, "Abrir a Ferramenta de Captura"),
        ]
    }
    fn event(&mut self, ev: Event) -> bool {
        let Event::Click(id) = ev else { return false };
        panel::close();
        // Espera o painel sumir da tela antes de capturar.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(250));
            let uri = match id {
                SHOT_AREA => "ms-screenclip://capture/image?rectangle",
                SHOT_WINDOW => "ms-screenclip://capture/image?window",
                SHOT_FULL => "ms-screenclip://capture/image?fullscreen",
                SHOT_VIDEO => "ms-screenclip://capture/video",
                _ => "ms-screensketch:",
            };
            let ok = unsafe { ShellExecuteW(None, w!("open"), &wide(uri), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) }.0 as isize > 32;
            // Ferramenta antiga, sem o protocolo novo: o atalho do teclado.
            if !ok && id != SHOT_APP {
                keys(&[VK_LWIN, VK_SHIFT, VK_S]);
            }
        });
        false
    }
}

// ---------------------------------------------------------------------------------------------
// Bateria e modo de energia

pub struct BatteryInfo {
    pub percent: u8,
    pub charging: bool,
    /// Segundos restantes na bateria (None = carregando ou desconhecido).
    pub left: Option<u32>,
}

pub fn battery() -> Option<BatteryInfo> {
    let mut s = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut s) }.ok()?;
    if s.BatteryFlag & 128 != 0 || s.BatteryFlag == 255 {
        return None;
    }
    let charging = s.ACLineStatus == 1;
    let left = (!charging && s.BatteryLifeTime != u32::MAX).then_some(s.BatteryLifeTime);
    Some(BatteryInfo { percent: s.BatteryLifePercent.min(100), charging, left })
}

pub fn duration_text(secs: u32) -> String {
    let (h, m) = (secs / 3600, secs / 60 % 60);
    if h > 0 { format!("{h} h {m:02} min") } else { format!("{m} min") }
}

/// Modos de energia do Windows 11 (sobreposições do plano).
const MODES: [(&str, GUID); 3] = [
    ("Melhor eficiência de energia", GUID::from_u128(0x961cc777_2547_4f9d_8174_7d86181b8a7a)),
    ("Equilibrado", GUID::zeroed()),
    ("Melhor desempenho", GUID::from_u128(0xded574b5_45a0_4f42_8737_46345c09c238)),
];

type GetOverlay = unsafe extern "system" fn(*mut GUID) -> u32;
type SetOverlay = unsafe extern "system" fn(GUID) -> u32;

fn powrprof(name: &[u8]) -> Option<unsafe extern "system" fn() -> isize> {
    unsafe {
        let lib = LoadLibraryW(w!("powrprof.dll")).ok()?;
        GetProcAddress(lib, windows::core::PCSTR(name.as_ptr()))
    }
}

fn power_mode() -> Option<usize> {
    let f: GetOverlay = unsafe { std::mem::transmute(powrprof(b"PowerGetEffectiveOverlayScheme\0")?) };
    let mut g = GUID::zeroed();
    (unsafe { f(&mut g) } == 0).then(|| MODES.iter().position(|m| m.1 == g)).flatten()
}

fn set_power_mode(i: usize) {
    if let Some(p) = powrprof(b"PowerSetActiveOverlayScheme\0") {
        let f: SetOverlay = unsafe { std::mem::transmute(p) };
        unsafe { f(MODES[i].1) };
    }
}

pub struct BatteryPanel;

impl PanelContent for BatteryPanel {
    fn width(&self) -> f32 {
        290.0
    }
    fn build(&mut self) -> Vec<Widget> {
        let mut out = Vec::new();
        if let Some(b) = battery() {
            let detail = if b.charging {
                if b.percent >= 100 { tr("Carregada") } else { tr("Carregando") }
            } else {
                b.left.map(|s| format!("{} {}", tr("Restam"), duration_text(s))).unwrap_or_else(|| tr("Na bateria"))
            };
            let sb = crate::status::Battery { percent: b.percent, charging: b.charging };
            out.push(Widget::Meter {
                glyph: crate::control::battery_glyph(&sb),
                label: tr("Bateria"),
                value: format!("{}%", b.percent),
                detail: Some(detail),
                frac: Some(b.percent as f32 / 100.0),
                level: if b.charging { 0 } else if b.percent <= 10 { 2 } else if b.percent <= 20 { 1 } else { 0 },
            });
        }
        if let Some(cur) = power_mode() {
            out.push(Widget::Label(tr("Modo de energia")));
            for (i, (name, _)) in MODES.iter().enumerate() {
                out.push(Widget::Row { id: 10 + i as u32, glyph: None, text: tr(name), sub: None, selected: i == cur });
            }
        }
        out.push(Widget::Separator);
        out.push(Widget::Row { id: 1, glyph: Some(glyph::SETTINGS), text: tr("Configurações de energia"), sub: None, selected: false });
        out
    }
    fn event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Click(1) => {
                panel::close();
                open("ms-settings:powersleep");
                false
            }
            Event::Click(id @ 10..=12) => {
                set_power_mode((id - 10) as usize);
                true
            }
            _ => false,
        }
    }
    fn notify(&mut self, msg: u32, _: usize) -> bool {
        msg == crate::control::TICK
    }
}

// ---------------------------------------------------------------------------------------------
// Notificações e Não incomodar

/// Avisos na central (-1 = o Windows não deixa ler).
static NOTIFY_COUNT: AtomicI32 = AtomicI32::new(-1);
static NOTIFY_BUSY: Mutex<bool> = Mutex::new(false);

pub fn notify_count() -> Option<u32> {
    let n = NOTIFY_COUNT.load(Ordering::Relaxed);
    (n >= 0).then_some(n as u32)
}

/// Relê o número de avisos numa thread (a API do Windows é assíncrona).
pub fn refresh_notify() {
    {
        let Ok(mut busy) = NOTIFY_BUSY.lock() else { return };
        if *busy {
            return;
        }
        *busy = true;
    }
    std::thread::spawn(|| {
        let n = read_notify_count().map_or(-1, |n| n as i32);
        let old = NOTIFY_COUNT.swap(n, Ordering::Relaxed);
        if let Ok(mut busy) = NOTIFY_BUSY.lock() {
            *busy = false;
        }
        if old != n {
            post_bar(3);
        }
    });
}

fn read_notify_count() -> Option<u32> {
    use windows::UI::Notifications::Management::{UserNotificationListener, UserNotificationListenerAccessStatus};
    use windows::UI::Notifications::NotificationKinds;
    let l = UserNotificationListener::Current().ok()?;
    if l.GetAccessStatus().ok()? != UserNotificationListenerAccessStatus::Allowed {
        return None;
    }
    let list = l.GetNotificationsAsync(NotificationKinds::Toast).ok()?.join().ok()?;
    list.Size().ok()
}

/// Abre a central de notificações (Win+N).
pub fn open_notifications() {
    keys(&[VK_LWIN, VK_N]);
}

#[repr(C)]
struct WnfName(u64);

type QueryWnf = unsafe extern "system" fn(*const WnfName, *const GUID, *const u8, *mut u32, *mut u8, *mut u32) -> i32;
type UpdateWnf = unsafe extern "system" fn(*const WnfName, *const u8, u32, *const GUID, *const u8, u32, u32) -> i32;

/// WNF_SHEL_QUIETHOURS_ACTIVE_PROFILE_CHANGED: perfil ativo do Não incomodar (0 = desligado).
const QUIET_HOURS: WnfName = WnfName(0x0D83063EA3BF1C75);

fn ntdll(name: &[u8]) -> Option<unsafe extern "system" fn() -> isize> {
    unsafe {
        let lib = LoadLibraryW(w!("ntdll.dll")).ok()?;
        GetProcAddress(lib, windows::core::PCSTR(name.as_ptr()))
    }
}

/// Não incomodar ligado? None = não deu para ler.
pub fn dnd() -> Option<bool> {
    let f: QueryWnf = unsafe { std::mem::transmute(ntdll(b"NtQueryWnfStateData\0")?) };
    let (mut stamp, mut buf, mut size) = (0u32, 0u32, 4u32);
    let st = unsafe {
        f(&QUIET_HOURS, std::ptr::null(), std::ptr::null(), &mut stamp, &mut buf as *mut u32 as *mut u8, &mut size)
    };
    (st >= 0).then_some(buf != 0)
}

/// Liga ou desliga o Não incomodar. Se o Windows não aceitar, abre a página dele.
pub fn set_dnd(on: bool) {
    let ok = (|| {
        let f: UpdateWnf = unsafe { std::mem::transmute(ntdll(b"NtUpdateWnfStateData\0")?) };
        // 1 = só prioridade (o "Não incomodar" do Windows 11); 0 = desligado.
        let v: u32 = on.into();
        let st = unsafe { f(&QUIET_HOURS, &v as *const u32 as *const u8, 4, std::ptr::null(), std::ptr::null(), 0, 0) };
        (st >= 0).then_some(())
    })()
    .is_some();
    std::thread::sleep(Duration::from_millis(60));
    if !ok || dnd() != Some(on) {
        open("ms-settings:notifications");
    }
    post_bar(3);
}

thread_local! {
    /// O timer de foco ligou o Não incomodar (e deve desligar no fim).
    static FOCUS_DND: Cell<bool> = const { Cell::new(false) };
}

/// Timer de foco começou ou acabou.
pub fn focus_timer(running: bool) {
    if !extras::on("bar_focus_dnd") {
        return;
    }
    if running && dnd() == Some(false) {
        FOCUS_DND.set(true);
        set_dnd(true);
    } else if !running && FOCUS_DND.replace(false) {
        set_dnd(false);
    }
}

// ---------------------------------------------------------------------------------------------
// Clima (Open-Meteo, sem conta)

#[derive(Clone)]
pub struct Weather {
    pub temp: f64,
    pub code: u32,
    pub day: bool,
    pub place: String,
    /// Próximos dias: (dia desde 1970, código, máxima, mínima).
    pub days: Vec<(i64, u32, f64, f64)>,
}

static WEATHER: Mutex<Option<Weather>> = Mutex::new(None);
static WEATHER_BUSY: Mutex<bool> = Mutex::new(false);

pub fn weather() -> Option<Weather> {
    WEATHER.lock().ok()?.clone()
}

/// Símbolo e nome do tempo pelo código WMO.
pub fn weather_text(code: u32, day: bool) -> (&'static str, &'static str) {
    match code {
        0 if day => ("☀", "Céu limpo"),
        0 => ("☾", "Céu limpo"),
        1 | 2 => ("⛅", "Poucas nuvens"),
        3 => ("☁", "Nublado"),
        45 | 48 => ("≋", "Neblina"),
        51..=57 => ("☂", "Garoa"),
        61..=67 | 80..=82 => ("☂", "Chuva"),
        71..=77 | 85 | 86 => ("❄", "Neve"),
        95..=99 => ("⚡", "Tempestade"),
        _ => ("☁", "—"),
    }
}

/// Valor de `"chave":` no texto JSON (número ou texto entre aspas).
fn json_value<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let i = s.find(&format!("\"{key}\":"))? + key.len() + 3;
    let rest = s[i..].trim_start();
    if let Some(r) = rest.strip_prefix('"') {
        return Some(&r[..r.find('"')?]);
    }
    let end = rest.find([',', '}', ']']).unwrap_or(rest.len());
    Some(rest[..end].trim())
}

fn json_array(s: &str, key: &str) -> Vec<String> {
    let Some(i) = s.find(&format!("\"{key}\":[")) else { return Vec::new() };
    let rest = &s[i + key.len() + 4..];
    let end = rest.find(']').unwrap_or(0);
    rest[..end].split(',').map(|v| v.trim().trim_matches('"').to_string()).collect()
}

fn get_json(host: &str, path: &str) -> Option<String> {
    let (status, body) = crate::usage::http_get(PCWSTR(wide(host).as_ptr()), PCWSTR(wide(path).as_ptr()), "")?;
    (status == 200).then(|| String::from_utf8_lossy(&body).into_owned())
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// Lugar do clima: (latitude, longitude, nome). Sem lugar escolhido, pelo IP.
fn weather_place() -> Option<(String, String, String)> {
    if let Some(v) = config::raw("weather_place") {
        let mut p = v.splitn(3, ';');
        if let (Some(a), Some(b), Some(c)) = (p.next(), p.next(), p.next()) {
            return Some((a.into(), b.into(), c.into()));
        }
    }
    let j = get_json("ipwho.is", "/?fields=latitude,longitude,city")?;
    Some((json_value(&j, "latitude")?.into(), json_value(&j, "longitude")?.into(), json_value(&j, "city").unwrap_or("").into()))
}

/// Procura a cidade e guarda como lugar do clima (vazio = volta a usar o IP).
pub fn set_weather_city(name: &str) {
    let name = name.trim().to_string();
    std::thread::spawn(move || {
        if name.is_empty() {
            config::set_raw("weather_place", String::new());
        } else {
            let path = format!("/v1/search?count=1&language=pt&name={}", url_encode(&name));
            let Some(j) = get_json("geocoding-api.open-meteo.com", &path) else { return };
            let (Some(lat), Some(lon)) = (json_value(&j, "latitude"), json_value(&j, "longitude")) else { return };
            let city = json_value(&j, "name").unwrap_or(&name).to_string();
            config::set_raw("weather_place", format!("{lat};{lon};{city}"));
        }
        fetch_weather_now();
    });
}

/// Busca o clima numa thread (uma de cada vez).
pub fn refresh_weather() {
    std::thread::spawn(fetch_weather_now);
}

fn fetch_weather_now() {
    {
        let Ok(mut busy) = WEATHER_BUSY.lock() else { return };
        if *busy {
            return;
        }
        *busy = true;
    }
    let w = (|| {
        let (lat, lon, place) = weather_place()?;
        let unit = if extras::choice("bar_temp_unit") == 1 { "&temperature_unit=fahrenheit" } else { "" };
        let path = format!(
            "/v1/forecast?latitude={lat}&longitude={lon}&current=temperature_2m,weather_code,is_day\
             &daily=weather_code,temperature_2m_max,temperature_2m_min&timezone=auto&forecast_days=5{unit}"
        );
        let j = get_json("api.open-meteo.com", &path)?;
        let cur = &j[j.find("\"current\":{")?..];
        let num = |k: &str| json_value(cur, k).and_then(|v| v.parse::<f64>().ok());
        let daily = &j[j.find("\"daily\":{")?..];
        let dates = json_array(daily, "time");
        let codes = json_array(daily, "weather_code");
        let maxs = json_array(daily, "temperature_2m_max");
        let mins = json_array(daily, "temperature_2m_min");
        let days = (0..dates.len())
            .filter_map(|i| {
                let d = &dates[i];
                let (y, m, dd) = (d.get(0..4)?.parse().ok()?, d.get(5..7)?.parse().ok()?, d.get(8..10)?.parse().ok()?);
                Some((days_from_civil(y, m, dd), codes.get(i)?.parse().ok()?, maxs.get(i)?.parse().ok()?, mins.get(i)?.parse().ok()?))
            })
            .collect();
        Some(Weather { temp: num("temperature_2m")?, code: num("weather_code")? as u32, day: num("is_day").unwrap_or(1.0) > 0.0, place, days })
    })();
    if let Some(w) = w
        && let Ok(mut slot) = WEATHER.lock()
    {
        *slot = Some(w);
    }
    if let Ok(mut busy) = WEATHER_BUSY.lock() {
        *busy = false;
    }
    post_bar(1);
}

const W_CITY: u32 = 1;
const W_HERE: u32 = 2;
const W_REFRESH: u32 = 3;

pub struct WeatherPanel;

impl PanelContent for WeatherPanel {
    fn width(&self) -> f32 {
        280.0
    }
    fn build(&mut self) -> Vec<Widget> {
        let mut out = Vec::new();
        match weather() {
            Some(w) => {
                let (sym, name) = weather_text(w.code, w.day);
                out.push(Widget::Title(format!("{sym}  {:.0}°", w.temp)));
                out.push(Widget::Text(if w.place.is_empty() { tr(name) } else { format!("{} · {}", w.place, tr(name)) }));
                out.push(Widget::Separator);
                let today = now_minutes().div_euclid(1440);
                for (day, code, max, min) in w.days.iter().filter(|d| d.0 >= today).take(5) {
                    let label = if *day == today { tr("Hoje") } else { tr(SHORT_DAYS[weekday_of(*day)]) };
                    out.push(Widget::Bar {
                        label: format!("{label}  {}", weather_text(*code, true).0),
                        value: format!("{max:.0}° / {min:.0}°"),
                        frac: ((max - min) / 20.0).clamp(0.1, 1.0) as f32,
                        strong: *day == today,
                    });
                }
            }
            None => out.push(Widget::Text(tr("Buscando o clima…"))),
        }
        out.push(Widget::Separator);
        out.push(Widget::Field { id: W_CITY, placeholder: tr("Outra cidade (Enter para usar)"), password: false });
        if config::raw("weather_place").is_some_and(|v| !v.is_empty()) {
            out.push(Widget::Row { id: W_HERE, glyph: Some(0xE81D), text: tr("Usar a minha localização"), sub: None, selected: false });
        }
        out.push(Widget::Row { id: W_REFRESH, glyph: Some(glyph::REFRESH), text: tr("Atualizar"), sub: None, selected: false });
        out
    }
    fn event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Submit(W_CITY, text) if !text.trim().is_empty() => {
                set_weather_city(text);
                false
            }
            Event::Click(W_HERE) => {
                set_weather_city("");
                false
            }
            Event::Click(W_REFRESH) => {
                refresh_weather();
                false
            }
            _ => false,
        }
    }
    fn notify(&mut self, msg: u32, wp: usize) -> bool {
        msg == WM_EXTRA && wp == 1
    }
}

// ---------------------------------------------------------------------------------------------
// Agenda (link iCal privado)

#[derive(Clone, Debug)]
pub struct CalEvent {
    /// Minutos desde 1970 (hora local).
    pub start: i64,
    pub end: i64,
    pub title: String,
    pub all_day: bool,
}

static EVENTS: Mutex<Option<Vec<CalEvent>>> = Mutex::new(None);
static EVENTS_BUSY: Mutex<bool> = Mutex::new(false);

pub fn events() -> Vec<CalEvent> {
    EVENTS.lock().ok().and_then(|e| e.clone()).unwrap_or_default()
}

pub fn has_ical() -> bool {
    config::raw("ical_url").is_some_and(|u| u.starts_with("https://"))
}

/// Busca a agenda numa thread (uma de cada vez).
pub fn refresh_events() {
    if !has_ical() {
        return;
    }
    {
        let Ok(mut busy) = EVENTS_BUSY.lock() else { return };
        if *busy {
            return;
        }
        *busy = true;
    }
    std::thread::spawn(|| {
        let list = config::raw("ical_url").and_then(|u| {
            let rest = u.strip_prefix("https://")?;
            let (host, path) = rest.split_at(rest.find('/')?);
            let (status, body) = crate::usage::http_get(PCWSTR(wide(host).as_ptr()), PCWSTR(wide(path).as_ptr()), "")?;
            (status == 200).then(|| parse_ics(&String::from_utf8_lossy(&body), now_minutes()))
        });
        if let Some(list) = list
            && let Ok(mut slot) = EVENTS.lock()
        {
            *slot = Some(list);
        }
        if let Ok(mut busy) = EVENTS_BUSY.lock() {
            *busy = false;
        }
        post_bar(2);
    });
}

/// "20261127T100000Z" / "20261127T100000" / "20261127" → (minutos locais, dia inteiro).
fn ics_time(value: &str, params: &str) -> Option<(i64, bool)> {
    let v = value.trim();
    let y: i64 = v.get(0..4)?.parse().ok()?;
    let mo: u32 = v.get(4..6)?.parse().ok()?;
    let d: u32 = v.get(6..8)?.parse().ok()?;
    if v.len() < 15 || params.contains("VALUE=DATE") && !params.contains("DATE-TIME") {
        return Some((days_from_civil(y, mo, d) * 1440, true));
    }
    let h: u16 = v.get(9..11)?.parse().ok()?;
    let mi: u16 = v.get(11..13)?.parse().ok()?;
    if v.ends_with('Z') {
        let utc = SYSTEMTIME { wYear: y as u16, wMonth: mo as u16, wDay: d as u16, wHour: h, wMinute: mi, ..Default::default() };
        let mut local = SYSTEMTIME::default();
        unsafe { SystemTimeToTzSpecificLocalTime(None, &utc, &mut local) }.ok()?;
        return Some((minutes_of(&local), false));
    }
    // Com TZID: tratado como hora local (o normal numa agenda do mesmo fuso).
    Some((days_from_civil(y, mo, d) * 1440 + h as i64 * 60 + mi as i64, false))
}

/// Eventos de ontem até daqui a 8 dias, com as repetições mais comuns (RRULE).
fn parse_ics(text: &str, now: i64) -> Vec<CalEvent> {
    // Linhas dobradas: começam com espaço ou tab e continuam a anterior.
    let mut lines: Vec<String> = Vec::new();
    for l in text.lines() {
        if (l.starts_with(' ') || l.starts_with('\t')) && !lines.is_empty() {
            lines.last_mut().unwrap().push_str(&l[1..]);
        } else {
            lines.push(l.to_string());
        }
    }
    let (from, to) = (now - 1440, now + 8 * 1440);
    let mut out = Vec::new();
    let mut overrides: Vec<(String, i64)> = Vec::new();
    let mut ev: Option<HashMap<String, (String, String)>> = None;
    let mut exdates: Vec<i64> = Vec::new();
    let mut raw: Vec<(HashMap<String, (String, String)>, Vec<i64>)> = Vec::new();
    for l in &lines {
        if l == "BEGIN:VEVENT" {
            ev = Some(HashMap::new());
            exdates.clear();
            continue;
        }
        if l == "END:VEVENT" {
            if let Some(e) = ev.take() {
                raw.push((e, std::mem::take(&mut exdates)));
            }
            continue;
        }
        let Some(e) = ev.as_mut() else { continue };
        let Some((head, value)) = l.split_once(':') else { continue };
        let (name, params) = head.split_once(';').unwrap_or((head, ""));
        if name == "EXDATE" {
            exdates.extend(value.split(',').filter_map(|v| ics_time(v, params)).map(|t| t.0));
        } else {
            e.insert(name.to_string(), (params.to_string(), value.to_string()));
        }
    }
    for (e, _) in &raw {
        if let (Some((_, uid)), Some((p, rid))) = (e.get("UID"), e.get("RECURRENCE-ID"))
            && let Some((t, _)) = ics_time(rid, p)
        {
            overrides.push((uid.clone(), t));
        }
    }
    for (e, ex) in raw {
        if e.get("STATUS").is_some_and(|s| s.1 == "CANCELLED") {
            continue;
        }
        let Some((p, v)) = e.get("DTSTART") else { continue };
        let Some((start, all_day)) = ics_time(v, p) else { continue };
        let end = e.get("DTEND").and_then(|(p, v)| ics_time(v, p)).map(|t| t.0).unwrap_or(start + if all_day { 1440 } else { 60 });
        let len = (end - start).max(0);
        let title = e
            .get("SUMMARY")
            .map(|s| s.1.replace("\\,", ",").replace("\\;", ";").replace("\\n", " "))
            .unwrap_or_else(|| tr("(sem título)"));
        let uid = e.get("UID").map(|u| u.1.clone()).unwrap_or_default();
        let is_override = e.contains_key("RECURRENCE-ID");
        let starts = match e.get("RRULE") {
            Some((_, rule)) if !is_override => expand(start, rule, from, to),
            _ => vec![start],
        };
        for s in starts {
            if s + len < from || s > to || ex.contains(&s) {
                continue;
            }
            if !is_override && e.contains_key("RRULE") && overrides.iter().any(|o| o.0 == uid && o.1 == s) {
                continue;
            }
            out.push(CalEvent { start: s, end: s + len, title: title.clone(), all_day });
        }
    }
    out.sort_by_key(|e| e.start);
    out
}

/// Inícios de uma regra de repetição dentro de [from, to].
fn expand(start: i64, rule: &str, from: i64, to: i64) -> Vec<i64> {
    let get = |k: &str| rule.split(';').find_map(|p| p.strip_prefix(&format!("{k}=")));
    let freq = get("FREQ").unwrap_or("");
    let interval: i64 = get("INTERVAL").and_then(|v| v.parse().ok()).unwrap_or(1).max(1);
    let count: Option<usize> = get("COUNT").and_then(|v| v.parse().ok());
    let until = get("UNTIL").and_then(|v| ics_time(v, "")).map(|t| t.0 + if v_is_date(get("UNTIL")) { 1439 } else { 0 });
    let byday: Vec<usize> = get("BYDAY")
        .map(|v| {
            v.split(',')
                .filter_map(|d| {
                    let d = d.trim_start_matches(|c: char| c == '-' || c == '+' || c.is_ascii_digit());
                    ["SU", "MO", "TU", "WE", "TH", "FR", "SA"].iter().position(|x| *x == d)
                })
                .collect()
        })
        .unwrap_or_default();
    let tod = start.rem_euclid(1440);
    let day0 = start.div_euclid(1440);
    let (y0, m0, d0) = civil_from_days(day0);
    let mut out = Vec::new();
    let mut n = 0usize;
    let past_end = |t: i64, n: usize| until.is_some_and(|u| t > u) || count.is_some_and(|c| n >= c) || t > to;
    match freq {
        "DAILY" => {
            let mut day = day0;
            for _ in 0..20000 {
                let t = day * 1440 + tod;
                if past_end(t, n) {
                    break;
                }
                if byday.is_empty() || byday.contains(&weekday_of(day)) {
                    n += 1;
                    if t >= from {
                        out.push(t);
                    }
                }
                day += interval;
            }
        }
        "WEEKLY" => {
            let days = if byday.is_empty() { vec![weekday_of(day0)] } else { byday };
            let week0 = day0 - weekday_of(day0) as i64;
            let mut week = week0;
            'w: for _ in 0..5000 {
                for wd in 0..7usize {
                    if !days.contains(&wd) {
                        continue;
                    }
                    let day = week + wd as i64;
                    if day < day0 {
                        continue;
                    }
                    let t = day * 1440 + tod;
                    if past_end(t, n) {
                        break 'w;
                    }
                    n += 1;
                    if t >= from {
                        out.push(t);
                    }
                }
                week += 7 * interval;
            }
        }
        "MONTHLY" | "YEARLY" => {
            let step = if freq == "YEARLY" { 12 * interval } else { interval };
            let mut k = 0i64;
            for _ in 0..2000 {
                let total = (m0 as i64 - 1) + k * step;
                let (y, m) = (y0 + total.div_euclid(12), (total.rem_euclid(12) + 1) as u32);
                k += 1;
                // Dia que não existe no mês (31 de abril) é pulado, como no Google.
                let (yy, mm, dd) = civil_from_days(days_from_civil(y, m, d0));
                if (yy, mm, dd) != (y, m, d0) {
                    continue;
                }
                let t = days_from_civil(y, m, d0) * 1440 + tod;
                if past_end(t, n) {
                    break;
                }
                n += 1;
                if t >= from {
                    out.push(t);
                }
            }
        }
        _ => out.push(start),
    }
    out
}

fn v_is_date(v: Option<&str>) -> bool {
    v.is_some_and(|v| v.len() == 8)
}

/// Próximo compromisso com hora (em andamento ou de hoje).
pub fn next_event() -> Option<CalEvent> {
    let now = now_minutes();
    let today_end = (now.div_euclid(1440) + 1) * 1440;
    events().into_iter().filter(|e| !e.all_day && e.end > now && e.start < today_end).min_by_key(|e| e.start)
}

/// Texto curto do próximo compromisso para a barra.
pub fn event_label() -> Option<String> {
    let e = next_event()?;
    let now = now_minutes();
    let mut title: String = e.title.chars().take(26).collect();
    if e.title.chars().count() > 26 {
        title.push('…');
    }
    Some(if e.start <= now {
        format!("{title} · {}", tr("agora"))
    } else if e.start - now <= 60 {
        format!("{title} {} {} min", tr("em"), e.start - now)
    } else {
        format!("{} {title}", hhmm(e.start))
    })
}

thread_local! {
    static ALERTED: RefCell<Vec<(i64, String)>> = const { RefCell::new(Vec::new()) };
}

/// Compromisso que chegou na hora do aviso (uma vez cada).
pub fn due_alert() -> Option<CalEvent> {
    let lead = extras::num("bar_event_alert") as i64;
    if lead <= 0 {
        return None;
    }
    let now = now_minutes();
    let e = events().into_iter().find(|e| !e.all_day && e.start > now - 1 && e.start - now <= lead)?;
    ALERTED.with_borrow_mut(|a| {
        if a.iter().any(|x| x.0 == e.start && x.1 == e.title) {
            return None;
        }
        a.retain(|x| x.0 > now - 1440);
        a.push((e.start, e.title.clone()));
        Some(e)
    })
}

const E_URL: u32 = 1;
const E_CHANGE: u32 = 2;
const E_REFRESH: u32 = 3;

pub struct AgendaPanel {
    editing: bool,
}

impl AgendaPanel {
    pub fn new() -> Self {
        Self { editing: !has_ical() }
    }
}

impl PanelContent for AgendaPanel {
    fn width(&self) -> f32 {
        300.0
    }
    fn centered(&self) -> bool {
        true
    }
    fn build(&mut self) -> Vec<Widget> {
        if self.editing {
            return vec![
                Widget::Heading { text: tr("Agenda"), buttons: Vec::new() },
                Widget::Text(tr(
                    "Cole o endereço secreto no formato iCal da sua agenda. No Google Agenda: Configurações › a sua agenda › Integrar agenda › Endereço secreto no formato iCal.",
                )),
                Widget::Field { id: E_URL, placeholder: "https://calendar.google.com/…/basic.ics".into(), password: true },
            ];
        }
        let now = now_minutes();
        let today = now.div_euclid(1440);
        let mut out = Vec::new();
        let list: Vec<CalEvent> = events().into_iter().filter(|e| e.end > now).take(10).collect();
        let mut last_day = None;
        for e in &list {
            let day = e.start.div_euclid(1440).max(today);
            if last_day != Some(day) {
                last_day = Some(day);
                let label = match day - today {
                    0 => tr("Hoje"),
                    1 => tr("Amanhã"),
                    _ => {
                        let (_, m, d) = civil_from_days(day);
                        format!("{} {d:02}/{m:02}", tr(SHORT_DAYS[weekday_of(day)]))
                    }
                };
                out.push(Widget::Label(label));
            }
            let sub = if e.all_day { tr("Dia todo") } else { format!("{} – {}", hhmm(e.start), hhmm(e.end)) };
            let current = !e.all_day && e.start <= now;
            out.push(Widget::Row { id: 0, glyph: Some(0xE787), text: e.title.clone(), sub: Some(sub), selected: current });
        }
        if list.is_empty() {
            out.push(Widget::Text(tr("Nenhum compromisso nos próximos dias")));
        }
        out.push(Widget::Separator);
        out.push(Widget::Row { id: E_REFRESH, glyph: Some(glyph::REFRESH), text: tr("Atualizar"), sub: None, selected: false });
        out.push(Widget::Row { id: E_CHANGE, glyph: Some(0xE71B), text: tr("Trocar o link da agenda"), sub: None, selected: false });
        out
    }
    fn event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Submit(E_URL, text) => {
                let url = text.trim().replace("webcal://", "https://");
                if url.starts_with("https://") {
                    config::set_raw("ical_url", url);
                    self.editing = false;
                    refresh_events();
                    return true;
                }
                false
            }
            Event::Click(E_CHANGE) => {
                self.editing = true;
                true
            }
            Event::Click(E_REFRESH) => {
                refresh_events();
                false
            }
            _ => false,
        }
    }
    fn notify(&mut self, msg: u32, wp: usize) -> bool {
        msg == WM_EXTRA && wp == 2
    }
}

// ---------------------------------------------------------------------------------------------
// Relógios mundiais

/// Cidades oferecidas e o fuso do Windows de cada uma.
pub const CITIES: [(&str, &str); 16] = [
    ("Nova York", "Eastern Standard Time"),
    ("Chicago", "Central Standard Time"),
    ("Los Angeles", "Pacific Standard Time"),
    ("Cidade do México", "Central Standard Time (Mexico)"),
    ("São Paulo", "E. South America Standard Time"),
    ("Buenos Aires", "Argentina Standard Time"),
    ("Lisboa", "GMT Standard Time"),
    ("Londres", "GMT Standard Time"),
    ("Paris", "Romance Standard Time"),
    ("Berlim", "W. Europe Standard Time"),
    ("Dubai", "Arabian Standard Time"),
    ("Nova Délhi", "India Standard Time"),
    ("Pequim", "China Standard Time"),
    ("Tóquio", "Tokyo Standard Time"),
    ("Sydney", "AUS Eastern Standard Time"),
    ("Auckland", "New Zealand Standard Time"),
];

pub fn chosen_cities() -> Vec<usize> {
    config::raw("world_clocks")
        .map(|v| v.split(',').filter_map(|s| s.parse().ok()).filter(|&i: &usize| i < CITIES.len()).collect())
        .unwrap_or_default()
}

pub fn toggle_city(i: usize) {
    let mut list = chosen_cities();
    if let Some(p) = list.iter().position(|&c| c == i) {
        list.remove(p);
    } else {
        list.push(i);
    }
    config::set_raw("world_clocks", list.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(","));
}

/// Hora agora na cidade `i`: (minutos desde 1970 lá, diferença em minutos para cá).
pub fn city_time(i: usize) -> Option<(i64, i64)> {
    let key = CITIES.get(i)?.1;
    let utc = unsafe { windows::Win32::System::SystemInformation::GetSystemTime() };
    let mut k = 0u32;
    loop {
        let mut tz = DYNAMIC_TIME_ZONE_INFORMATION::default();
        if unsafe { EnumDynamicTimeZoneInformation(k, &mut tz) } != 0 {
            return None;
        }
        k += 1;
        let n = tz.TimeZoneKeyName.iter().position(|&c| c == 0).unwrap_or(tz.TimeZoneKeyName.len());
        if String::from_utf16_lossy(&tz.TimeZoneKeyName[..n]) != key {
            continue;
        }
        let mut there = SYSTEMTIME::default();
        unsafe { SystemTimeToTzSpecificLocalTimeEx(Some(&tz), &utc, &mut there) }.ok()?;
        let t = minutes_of(&there);
        return Some((t, t - now_minutes()));
    }
}

/// Linhas do calendário para os relógios mundiais.
pub fn world_rows(base_id: u32) -> Vec<Widget> {
    let today = now_minutes().div_euclid(1440);
    chosen_cities()
        .into_iter()
        .filter_map(|i| {
            let (t, diff) = city_time(i)?;
            let day = match t.div_euclid(1440) - today {
                0 => tr("Hoje"),
                1 => tr("Amanhã"),
                -1 => tr("Ontem"),
                _ => String::new(),
            };
            let h = diff as f64 / 60.0;
            let off = if diff == 0 {
                tr("mesma hora")
            } else {
                let v = if h.fract() == 0.0 { format!("{}", h.abs()) } else { format!("{:.1}", h.abs()).replace('.', ",") };
                format!("{}{v} h", if diff > 0 { "+" } else { "−" })
            };
            Some(Widget::Row {
                id: base_id + i as u32,
                glyph: Some(0xE12B),
                text: format!("{}  {}", hhmm(t), CITIES[i].0),
                sub: Some(format!("{day} · {off}")),
                selected: false,
            })
        })
        .collect()
}
