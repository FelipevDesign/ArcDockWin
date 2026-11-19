//! Apps do dock: fixados (configuração) e abertos, ícones e ações de clique.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tiny_skia::Pixmap;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, SIZE};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits,
    GetObjectW, HBITMAP, HGDIOBJ, ReleaseDC,
};
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_ICONONLY, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetClassNameW, GetForegroundWindow, GetWindow,
    GetWindowLongW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    SW_MINIMIZE, SW_RESTORE, SW_SHOWNORMAL, SetForegroundWindow, ShowWindow, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, HSTRING, PWSTR, w};

const APPS_FOLDER: &str = "shell:AppsFolder\\";

pub struct App {
    /// Caminho do .exe ou `shell:AppsFolder\<id>` (apps da Store).
    pub source: String,
    /// Identifica o app ao casar com as janelas abertas.
    pub key: String,
    pub pinned: bool,
    pub icon: Option<Pixmap>,
    /// Cor pastel da caixinha atrás do ícone (RGBA), tirada da cor predominante dele.
    pub tile: [u8; 4],
    /// O ícone já é um "azulejo" (quadrado e preenchido): ocupa a caixa inteira, sem fundo pastel.
    pub full: bool,
    /// Silhueta preta do ícone, para a sombra leve dentro da caixinha.
    pub shadow: Option<Pixmap>,
    /// Janelas abertas do app, na ordem em que apareceram.
    pub windows: Vec<HWND>,
}

const DEFAULT_CONFIG: &str = "\
# ArcDockWin - apps fixados no dock
# Um caminho de .exe por linha (ou shell:AppsFolder\\<id> para apps da Store).
# Linhas começando com # são ignoradas.
C:\\Windows\\explorer.exe
C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe
C:\\Windows\\System32\\notepad.exe
";

pub fn config_path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default();
    base.join("ArcDockWin").join("pinned.txt")
}

pub fn load_pinned(icon_size: i32) -> Vec<App> {
    let cfg = config_path();
    if !cfg.exists() {
        if let Some(dir) = cfg.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&cfg, DEFAULT_CONFIG);
    }
    let text = std::fs::read_to_string(&cfg).unwrap_or_else(|_| DEFAULT_CONFIG.to_string());

    let start = App {
        source: START.to_string(),
        key: START.to_string(),
        pinned: true,
        icon: start_icon(icon_size as u32),
        tile: [0; 4],
        full: false,
        shadow: None,
        windows: Vec::new(),
    };
    // O botão Iniciar fica na ponta direita, depois dos fixados e dos abertos.
    let mut apps: Vec<App> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter(|l| l.starts_with(APPS_FOLDER) || Path::new(l).exists())
        .map(|source| App {
            key: source_key(source),
            icon: load_icon(source, icon_size),
            tile: [0; 4],
            full: false,
            shadow: None,
            source: source.to_string(),
            pinned: true,
            windows: Vec::new(),
        })
        .chain(std::iter::once(start))
        .collect();
    for app in &mut apps {
        app.tile = pastel(app.icon.as_ref());
        app.full = is_tile(app.icon.as_ref());
        app.shadow = silhouette(app.icon.as_ref());
    }
    apps
}

/// O ícone (já sem margens) é quase quadrado, quase todo opaco e com as bordas preenchidas?
pub fn is_tile(icon: Option<&Pixmap>) -> bool {
    let Some(pm) = icon else { return false };
    let (w, h) = (pm.width(), pm.height());
    if w < 8 || h < 8 || !(0.9..=1.1).contains(&(w as f32 / h as f32)) {
        return false;
    }
    let alpha = |x: u32, y: u32| pm.data()[((y * w + x) * 4 + 3) as usize];
    let edges = [(w / 2, 1), (w / 2, h - 2), (1, h / 2), (w - 2, h / 2)];
    if edges.iter().any(|&(x, y)| alpha(x, y) < 200) {
        return false;
    }
    let opaque = pm.data().chunks_exact(4).filter(|p| p[3] >= 200).count();
    opaque as f32 / (w * h) as f32 >= 0.85
}

/// Tom pastel da cor predominante do ícone (pixels vivos pesam mais que preto/branco/cinza).
/// Cópia do ícone toda preta, mantendo o alfa (pré-multiplicado: RGB = 0).
pub fn silhouette(icon: Option<&Pixmap>) -> Option<Pixmap> {
    let mut out = icon?.clone();
    for p in out.data_mut().chunks_exact_mut(4) {
        p[0] = 0;
        p[1] = 0;
        p[2] = 0;
    }
    Some(out)
}

pub fn pastel(icon: Option<&Pixmap>) -> [u8; 4] {
    let (mut x, mut y, mut wsum, mut n, mut vivid) = (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for p in icon.map(|i| i.data()).unwrap_or(&[]).chunks_exact(4) {
        let a = p[3] as f32;
        if a < 128.0 {
            continue;
        }
        let [r, g, b] = [p[0], p[1], p[2]].map(|c| c as f32 / a); // tira a pré-multiplicação
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        n += 1.0;
        if max - min < 1e-3 {
            continue;
        }
        let hue = if max == r {
            ((g - b) / (max - min)).rem_euclid(6.0)
        } else if max == g {
            (b - r) / (max - min) + 2.0
        } else {
            (r - g) / (max - min) + 4.0
        } * std::f32::consts::FRAC_PI_3;
        let w = (max - min) / max * max; // saturação × brilho
        if (max - min) / max > 0.25 && max > 0.2 {
            vivid += 1.0;
        }
        x += w * hue.cos();
        y += w * hue.sin();
        wsum += w;
    }
    // Pouca área colorida (ex.: engrenagem com um detalhe azul): cinza claro neutro.
    // Senão, um pastel bem suave, só um toque da cor do ícone.
    let (hue, sat, light) = if n == 0.0 || wsum / n < 0.08 || vivid / n < 0.40 {
        (0.0, 0.0, 0.94)
    } else {
        (y.atan2(x).to_degrees().rem_euclid(360.0), 0.22, 0.92)
    };
    let [r, g, b] = hsl_to_rgb(hue, sat, light);
    [r, g, b, 255]
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r, g, b].map(|v| ((v + m) * 255.0).round() as u8)
}

/// Item especial do botão Iniciar.
pub const START: &str = "::start";

/// Logo do Windows: quatro quadrados com degradê azul.
fn start_icon(size: u32) -> Option<Pixmap> {
    use tiny_skia::{GradientStop, LinearGradient, Paint, Point, Rect, SpreadMode, Transform};
    let mut pm = Pixmap::new(size, size)?;
    let s = size as f32;
    let gap = s * 0.04;
    let half = (s - gap) / 2.0;
    let shader = LinearGradient::new(
        Point::from_xy(0.0, 0.0),
        Point::from_xy(s, s),
        vec![
            GradientStop::new(0.0, tiny_skia::Color::from_rgba8(0x2A, 0xC0, 0xFA, 255)),
            GradientStop::new(1.0, tiny_skia::Color::from_rgba8(0x00, 0x5F, 0xC4, 255)),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    )?;
    let paint = Paint { shader, anti_alias: true, ..Default::default() };
    for (x, y) in [(0.0, 0.0), (half + gap, 0.0), (0.0, half + gap), (half + gap, half + gap)] {
        if let Some(r) = Rect::from_xywh(x, y, half, half) {
            pm.fill_rect(r, &paint, Transform::identity(), None);
        }
    }
    Some(pm)
}

/// Processos rodando agora (nome do exe em minúsculas).
fn running_exes() -> Vec<String> {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            out.push(String::from_utf16_lossy(&e.szExeFile[..n]).to_lowercase());
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// Botão Iniciar do dock: abre a Paleta de Comandos do PowerToys. Se o PowerToys ou a Paleta
/// estão fechados, abre antes (a Paleta aberta pelo link `x-cmdpal:` já aparece na tela);
/// com ela rodando, aperta o atalho dela (Win+Alt+Espaço).
fn open_start() {
    let running = running_exes();
    if !running.iter().any(|n| n == "powertoys.exe")
        && let Some(local) = std::env::var_os("LOCALAPPDATA")
    {
        let exe = std::path::PathBuf::from(local).join(r"PowerToys\PowerToys.exe");
        if exe.exists() {
            unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(exe.as_os_str()), None, None, SW_SHOWNORMAL) };
        }
    }
    if !running.iter().any(|n| n == "microsoft.cmdpal.ui.exe") {
        unsafe { ShellExecuteW(None, w!("open"), w!("x-cmdpal:"), None, None, SW_SHOWNORMAL) };
        return;
    }
    send_palette_hotkey();
}

/// Win+Alt+Espaço, o atalho da Paleta de Comandos.
fn send_palette_hotkey() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
        VIRTUAL_KEY, VK_LMENU, VK_LWIN, VK_SPACE,
    };
    let key = |vk: VIRTUAL_KEY, flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, dwFlags: flags, ..Default::default() } },
    };
    let (down, up) = (KEYBD_EVENT_FLAGS(0), KEYEVENTF_KEYUP);
    let seq = [
        key(VK_LWIN, down),
        key(VK_LMENU, down),
        key(VK_SPACE, down),
        key(VK_SPACE, up),
        key(VK_LMENU, up),
        key(VK_LWIN, up),
    ];
    unsafe {
        SendInput(&seq, size_of::<INPUT>() as i32);
    }
}

fn source_key(source: &str) -> String {
    match source.strip_prefix(APPS_FOLDER) {
        Some(id) => format!("aumid:{}", id.to_lowercase()),
        None => exe_key(source),
    }
}

fn exe_key(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Ícone em alta resolução, como pixmap RGBA pré-multiplicado.
fn load_icon(source: &str, size: i32) -> Option<Pixmap> {
    unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(&HSTRING::from(source), None).ok()?;
        let hbmp = factory
            .GetImage(SIZE { cx: size, cy: size }, SIIGBF_ICONONLY)
            .ok()?;
        let pixmap = hbitmap_to_pixmap(hbmp);
        let _ = DeleteObject(HGDIOBJ(hbmp.0));
        pixmap.and_then(trim_transparent)
    }
}

/// Corta as margens transparentes do ícone, para todos ocuparem a caixa por igual.
fn trim_transparent(pm: Pixmap) -> Option<Pixmap> {
    let (w, h) = (pm.width(), pm.height());
    let alpha = |x: u32, y: u32| pm.data()[((y * w + x) * 4 + 3) as usize];
    let (mut l, mut t, mut r, mut b) = (w, h, 0, 0);
    for y in 0..h {
        for x in 0..w {
            if alpha(x, y) > 8 {
                l = l.min(x);
                t = t.min(y);
                r = r.max(x + 1);
                b = b.max(y + 1);
            }
        }
    }
    if l >= r || t >= b {
        return Some(pm);
    }
    let rect = tiny_skia::IntRect::from_ltrb(l as i32, t as i32, r as i32, b as i32)?;
    pm.clone_rect(rect)
}

unsafe fn hbitmap_to_pixmap(hbmp: HBITMAP) -> Option<Pixmap> {
    unsafe {
        let mut bm = BITMAP::default();
        if GetObjectW(
            HGDIOBJ(hbmp.0),
            size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut _ as *mut _),
        ) == 0
        {
            return None;
        }
        let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // de cima para baixo
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let hdc = GetDC(None);
        let lines = GetDIBits(
            hdc,
            hbmp,
            0,
            h as u32,
            Some(buf.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        ReleaseDC(None, hdc);
        if lines == 0 {
            return None;
        }

        // BGRA -> RGBA. Se algum canal passar do alfa, a imagem não está pré-multiplicada.
        let straight = buf
            .chunks_exact(4)
            .any(|p| p[0] > p[3] || p[1] > p[3] || p[2] > p[3]);
        for p in buf.chunks_exact_mut(4) {
            p.swap(0, 2);
            if straight {
                let a = p[3] as u16;
                for c in &mut p[..3] {
                    *c = ((*c as u16 * a + 127) / 255) as u8;
                }
            }
        }
        Pixmap::from_vec(buf, tiny_skia::IntSize::from_wh(w as u32, h as u32)?)
    }
}

/// Atualiza as janelas de cada app e a lista de apps abertos não fixados.
/// Retorna true se algo mudou.
pub fn refresh(apps: &mut Vec<App>, icon_size: i32) -> bool {
    let mut hwnds: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut hwnds as *mut _ as isize));
    }

    let mut procs: HashMap<u32, String> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, (String, Vec<HWND>)> = HashMap::new();
    for hwnd in hwnds {
        let Some((key, source)) = window_app(hwnd, &mut procs) else { continue };
        groups
            .entry(key.clone())
            .or_insert_with(|| {
                order.push(key);
                (source, Vec::new())
            })
            .1
            .push(hwnd);
    }

    let mut changed = false;
    for app in apps.iter_mut() {
        let found = groups.remove(&app.key).map(|g| g.1).unwrap_or_default();
        // Ordem estável: as que já estavam ficam no lugar, as novas vão para o fim.
        let mut new: Vec<HWND> = app.windows.iter().copied().filter(|h| found.contains(h)).collect();
        new.extend(found.into_iter().filter(|h| !app.windows.contains(h)));
        if new != app.windows {
            changed = true;
            app.windows = new;
        }
    }

    let before = apps.len();
    apps.retain(|a| a.pinned || !a.windows.is_empty());
    changed |= apps.len() != before;

    for key in order {
        if let Some((source, windows)) = groups.remove(&key) {
            let icon = load_icon(&source, icon_size);
            // Antes do botão Iniciar, que é sempre o último.
            let at = apps.iter().position(|a| a.source == START).unwrap_or(apps.len());
            apps.insert(at, App {
                tile: pastel(icon.as_ref()),
                full: is_tile(icon.as_ref()),
                shadow: silhouette(icon.as_ref()),
                icon,
                source,
                key,
                pinned: false,
                windows,
            });
            changed = true;
        }
    }
    changed
}

/// (key, source) do app dono da janela.
fn window_app(hwnd: HWND, procs: &mut HashMap<u32, String>) -> Option<(String, String)> {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    let path = procs.entry(pid).or_insert_with(|| process_path(pid)).clone();
    let key = exe_key(&path);
    if key.is_empty() {
        return None;
    }
    if key == "applicationframehost.exe" {
        // Apps da Store rodam dentro do ApplicationFrameHost; o app real vem do AppUserModelID.
        let id = window_aumid(hwnd)?;
        return Some((format!("aumid:{}", id.to_lowercase()), format!("{APPS_FOLDER}{id}")));
    }
    Some((key, path))
}

fn window_aumid(hwnd: HWND) -> Option<String> {
    unsafe {
        let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd).ok()?;
        let value = store.GetValue(&PKEY_AppUserModel_ID).ok()?;
        let id = value.to_string();
        (!id.is_empty()).then_some(id)
    }
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        if is_app_window(hwnd) {
            (*(lparam.0 as *mut Vec<HWND>)).push(hwnd);
        }
    }
    BOOL(1)
}

/// Janelas que apareceriam na barra de tarefas.
unsafe fn is_app_window(hwnd: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || GetWindowTextLengthW(hwnd) == 0 {
            return false;
        }
        if GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid()) {
            return false;
        }
        // As janelas do próprio dock (topbar, painéis...) não são apps.
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == windows::Win32::System::Threading::GetCurrentProcessId() {
            return false;
        }
        if GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
            return false;
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut _ as *mut _,
            size_of::<u32>() as u32,
        );
        if cloaked != 0 {
            return false;
        }
        let mut class = [0u16; 64];
        let len = GetClassNameW(hwnd, &mut class) as usize;
        let class = String::from_utf16_lossy(&class[..len]);
        !matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd")
    }
}

fn process_path(pid: u32) -> String {
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(handle);
        if ok { String::from_utf16_lossy(&buf[..len as usize]) } else { String::new() }
    }
}

/// Clique no ícone: abre o app, alterna entre as janelas dele ou minimiza a única em foco.
pub fn activate(source: &str, windows: &[HWND]) {
    if source == START {
        open_start();
        return;
    }
    unsafe {
        if windows.is_empty() {
            ShellExecuteW(None, w!("open"), &HSTRING::from(source), None, None, SW_SHOWNORMAL);
            return;
        }
        let fg = GetForegroundWindow();
        match windows.iter().position(|&h| h == fg) {
            Some(_) if windows.len() == 1 => {
                let _ = ShowWindow(fg, SW_MINIMIZE);
            }
            // Várias janelas: a próxima na ordem estável, em ciclo.
            Some(i) => focus_window(windows[(i + 1) % windows.len()]),
            // Nenhuma em foco: a mais recente não minimizada, ou a primeira se todas estiverem.
            None => {
                let z = z_order(windows);
                let top = z.iter().copied().find(|&h| !IsIconic(h).as_bool());
                focus_window(top.or(z.first().copied()).unwrap_or(windows[0]));
            }
        }
    }
}

/// Traz a janela para frente, restaurando se estiver minimizada.
pub fn focus_window(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

/// As janelas dadas, da mais à frente para a mais atrás.
fn z_order(windows: &[HWND]) -> Vec<HWND> {
    let mut all: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut all as *mut _ as isize));
    }
    all.retain(|h| windows.contains(h));
    all
}

/// Título da janela para o menu, cortado em ~60 caracteres.
pub fn menu_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) }.max(0) as usize;
    let title = String::from_utf16_lossy(&buf[..len]);
    let mut out: String = title.chars().take(60).collect();
    if title.chars().count() > 60 {
        out.pop();
        out.push('…');
    }
    // & no menu vira atalho de teclado: dobra para aparecer como texto.
    out.replace('&', "&&")
}

/// Fixa ou desafixa o app no dock e no pinned.txt (comentários e demais linhas ficam como estão).
/// Fixados ficam antes dos abertos: o recém-fixado vai para o fim do grupo dos fixados.
pub fn set_pinned(apps: &mut Vec<App>, key: &str, pin: bool) {
    let Some(i) = apps.iter().position(|a| a.key == key) else { return };
    if apps[i].pinned == pin || apps[i].source == START {
        return;
    }
    let cfg = config_path();
    let mut text = std::fs::read_to_string(&cfg).unwrap_or_default();
    let source = apps[i].source.clone();
    if pin {
        let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
        if !text.is_empty() && !text.ends_with('\n') {
            text.push_str(nl);
        }
        text.push_str(&source);
        text.push_str(nl);
    } else {
        text = text.split_inclusive('\n').filter(|l| l.trim() != source).collect();
    }
    if let Some(dir) = cfg.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&cfg, text);

    let mut app = apps.remove(i);
    app.pinned = pin;
    if pin || !app.windows.is_empty() {
        let at = apps.iter().filter(|a| a.pinned && a.source != START).count();
        apps.insert(at, app);
    }
}
