//! Janela de pré-visualização (processo à parte: `arcdockwin.exe --peek <lista>`), aberta pelo
//! Espaço no Explorador (`quicklook.rs`). Mostra imagens (WIC, já na orientação certa), texto e
//! código, pastas, conteúdo de .zip, vídeo e áudio (Media Foundation) e, para o resto (PDF,
//! Office...), os mesmos pré-visualizadores do painel de visualização do Explorador. Sem nenhum
//! deles, a miniatura do Windows e os detalhes do arquivo.
//!
//! Teclas: Esc ou Espaço fecham; ← → (ou Alt+← →) trocam de arquivo; Enter abre no programa
//! padrão; Ctrl+C copia o arquivo. Clique no vídeo pausa/continua.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{COLORREF, GENERIC_READ, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::Media::MediaFoundation::{IMFPMediaPlayer, MFPCreateMediaPlayer, MFStartup, MF_VERSION};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, STGM_READ,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{HKEY_CLASSES_ROOT, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::UI::Controls::SetWindowTheme;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus, VK_CONTROL, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SPACE, VK_UP};
use windows::Win32::UI::Shell::PropertiesSystem::{IInitializeWithFile, IInitializeWithStream};
use windows::Win32::UI::Shell::{
    IInitializeWithItem, IPreviewHandler, IShellItem, IShellItemImageFactory, SHCreateItemFromParsingName, SHCreateStreamOnFileEx,
    SIIGBF_BIGGERSIZEOK, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{GUID, HSTRING, Interface, PCWSTR, w};

const BG: u32 = 0x001F1B1B; // COLORREF 0x00BBGGRR
const BAR: i32 = 34;
const WM_RELOAD: u32 = WM_APP + 1;

const IMAGES: &[&str] = &["png", "jpg", "jpeg", "jfif", "gif", "bmp", "webp", "heic", "heif", "avif", "tif", "tiff", "ico", "jxr", "dds"];
const MEDIA: &[&str] = &["mp4", "m4v", "mov", "mkv", "webm", "avi", "wmv", "mp3", "wav", "m4a", "aac", "flac", "wma", "ogg", "opus", "3gp"];
const AUDIO: &[&str] = &["mp3", "wav", "m4a", "aac", "flac", "wma", "ogg", "opus"];
const TEXT: &[&str] = &[
    "txt", "md", "markdown", "json", "jsonc", "xml", "csv", "tsv", "log", "ini", "cfg", "conf", "yml", "yaml", "toml", "rs", "py", "js", "mjs",
    "ts", "tsx", "jsx", "html", "htm", "css", "scss", "c", "h", "cpp", "hpp", "cc", "cs", "java", "kt", "go", "rb", "php", "ps1", "psm1",
    "bat", "cmd", "sh", "sql", "lua", "r", "swift", "dart", "vue", "svelte", "gitignore", "env", "srt", "vtt", "reg", "tsv", "properties",
];

static EDIT: AtomicIsize = AtomicIsize::new(0);
static CONTENT: AtomicIsize = AtomicIsize::new(0);
static EDIT_BRUSH: AtomicIsize = AtomicIsize::new(0);

#[derive(Default)]
struct View {
    hwnd: HWND,
    list: Vec<PathBuf>,
    index: usize,
    /// Imagem já no tamanho de mostrar (BGRA pré-multiplicado), largura e altura.
    image: Option<(Vec<u8>, i32, i32)>,
    /// Texto sob a imagem (miniatura de arquivos sem pré-visualizador).
    caption: String,
    info: String,
    handler: Option<IPreviewHandler>,
    player: Option<IMFPMediaPlayer>,
    paused: bool,
    font: HFONT,
    ui_font: HFONT,
}

thread_local! {
    static VIEW: RefCell<View> = RefCell::new(View::default());
}

fn with<R>(f: impl FnOnce(&mut View) -> R) -> Option<R> {
    VIEW.with(|v| v.try_borrow_mut().ok().map(|mut v| f(&mut v)))
}

/// `arcdockwin.exe --peek <arquivo com a lista>`: roda a janela e devolve true (o processo sai).
pub fn run_if_requested() -> bool {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some("--peek") {
        return false;
    }
    if let Some(list) = args.get(2) {
        run(Path::new(list));
    }
    true
}

fn read_list(file: &Path) -> (Vec<PathBuf>, usize) {
    let text = std::fs::read_to_string(file).unwrap_or_default();
    let mut lines = text.lines();
    let index = lines.next().and_then(|l| l.trim().parse().ok()).unwrap_or(0);
    let list: Vec<PathBuf> = lines.filter(|l| !l.trim().is_empty()).map(PathBuf::from).collect();
    let index = index.min(list.len().saturating_sub(1));
    (list, index)
}

fn run(file: &Path) {
    crate::i18n::initialize();
    let (list, index) = read_list(file);
    if list.is_empty() {
        return;
    }
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let _ = MFStartup(MF_VERSION, 0);
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let bg = CreateSolidBrush(COLORREF(BG));
        EDIT_BRUSH.store(bg.0 as isize, Ordering::Relaxed);
        let arrow = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(main_proc),
            hInstance: inst.into(),
            hCursor: arrow,
            hbrBackground: bg,
            lpszClassName: w!("ArcDockPeek"),
            hIcon: LoadIconW(Some(inst.into()), PCWSTR(1 as _)).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let wc2 = WNDCLASSW { lpfnWndProc: Some(content_proc), hInstance: inst.into(), hCursor: arrow, hbrBackground: bg, lpszClassName: w!("ArcDockPeekContent"), ..Default::default() };
        RegisterClassW(&wc2);

        // Centralizada no monitor do cursor, no tamanho escolhido.
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let wa = mi.rcWork;
        let k = crate::extras::num("ql_size") / 100.0;
        let (ww, wh) = (((wa.right - wa.left) as f32 * k) as i32, ((wa.bottom - wa.top) as f32 * k) as i32);
        let (x, y) = (wa.left + (wa.right - wa.left - ww) / 2, wa.top + (wa.bottom - wa.top - wh) / 2);
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("ArcDockPeek"),
            w!("Pré-visualização"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            x,
            y,
            ww,
            wh,
            None,
            None,
            Some(inst.into()),
            None,
        ) else {
            return;
        };
        let dark = windows::core::BOOL(1);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
        let content = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("ArcDockPeekContent"),
            w!(""),
            WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN,
            0,
            0,
            10,
            10,
            Some(hwnd),
            None,
            Some(inst.into()),
            None,
        )
        .unwrap_or_default();
        CONTENT.store(content.0 as isize, Ordering::Relaxed);
        let edit = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("EDIT"),
            w!(""),
            WS_CHILD | WS_VSCROLL | WS_HSCROLL | WINDOW_STYLE((ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL | ES_AUTOHSCROLL) as u32),
            0,
            0,
            10,
            10,
            Some(content),
            None,
            Some(inst.into()),
            None,
        )
        .unwrap_or_default();
        let _ = SetWindowTheme(edit, w!("DarkMode_Explorer"), PCWSTR::null());
        SendMessageW(edit, 0x00C5 /* EM_SETLIMITTEXT */, Some(WPARAM(0x7FFF_FFFE)), None);
        EDIT.store(edit.0 as isize, Ordering::Relaxed);

        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
        let px = |v: i32| -(v * dpi / 96);
        let font = CreateFontW(px(14), 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, FIXED_PITCH.0 as u32, w!("Cascadia Mono"));
        let ui_font = CreateFontW(px(13), 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
        SendMessageW(edit, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));

        VIEW.with(|v| {
            let mut v = v.borrow_mut();
            v.hwnd = hwnd;
            v.list = list;
            v.index = index;
            v.font = font;
            v.ui_font = ui_font;
        });
        layout(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);
        bring_front(hwnd);
        load();

        let list_file = file.to_path_buf();
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.message == WM_RELOAD {
                let (list, index) = read_list(&list_file);
                if !list.is_empty() {
                    with(|v| {
                        v.list = list;
                        v.index = index;
                    });
                    load();
                }
                continue;
            }
            if (msg.message == WM_KEYDOWN || msg.message == WM_SYSKEYDOWN) && key(hwnd, msg.wParam.0 as u16) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        with(|v| {
            if let Some(h) = v.handler.take() {
                let _ = h.Unload();
            }
            if let Some(p) = v.player.take() {
                let _ = p.Shutdown();
            }
        });
    }
}

/// Primeiro plano mesmo aberta por outro processo (pega a fila de entrada emprestada).
fn bring_front(hwnd: HWND) {
    unsafe {
        if SetForegroundWindow(hwnd).as_bool() {
            return;
        }
        use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
        let fg = GetWindowThreadProcessId(GetForegroundWindow(), None);
        let me = GetCurrentThreadId();
        if fg != 0 && AttachThreadInput(me, fg, true).as_bool() {
            let _ = SetForegroundWindow(hwnd);
            let _ = AttachThreadInput(me, fg, false);
        }
    }
}

/// Teclas da janela (vêm antes dos controles filhos). true = tratada.
fn key(hwnd: HWND, vk: u16) -> bool {
    let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
    match vk {
        v if v == VK_ESCAPE.0 || v == VK_SPACE.0 => unsafe {
            let _ = DestroyWindow(hwnd);
            true
        },
        v if v == VK_LEFT.0 || v == VK_RIGHT.0 => {
            let d: isize = if v == VK_LEFT.0 { -1 } else { 1 };
            let go = with(|v| {
                let n = v.list.len() as isize;
                let i = (v.index as isize + d).rem_euclid(n.max(1)) as usize;
                let changed = i != v.index;
                v.index = i;
                changed
            })
            .unwrap_or(false);
            if go {
                load();
            }
            true
        }
        v if v == VK_RETURN.0 => {
            if let Some(p) = with(|v| v.list.get(v.index).cloned()).flatten() {
                unsafe {
                    ShellExecuteW(None, w!("open"), &HSTRING::from(p.as_os_str()), None, None, SW_SHOWNORMAL);
                    let _ = DestroyWindow(hwnd);
                }
            }
            true
        }
        0x43 if ctrl => {
            if let Some(p) = with(|v| v.list.get(v.index).cloned()).flatten() {
                crate::clipboard::set_files(&[p]);
            }
            true
        }
        // Rolagem do texto, mesmo sem foco na caixa.
        v if [VK_UP.0, VK_DOWN.0, VK_PRIOR.0, VK_NEXT.0, VK_HOME.0, VK_END.0].contains(&v) => unsafe {
            let edit = HWND(EDIT.load(Ordering::Relaxed) as _);
            if IsWindowVisible(edit).as_bool() {
                let code = match v {
                    x if x == VK_UP.0 => SB_LINEUP,
                    x if x == VK_DOWN.0 => SB_LINEDOWN,
                    x if x == VK_PRIOR.0 => SB_PAGEUP,
                    x if x == VK_NEXT.0 => SB_PAGEDOWN,
                    x if x == VK_HOME.0 => SB_TOP,
                    _ => SB_BOTTOM,
                };
                SendMessageW(edit, WM_VSCROLL, Some(WPARAM(code.0 as usize)), None);
            }
            true
        },
        _ => false,
    }
}

/// Área do conteúdo (a janela toda menos a barra de baixo).
fn content_rect(hwnd: HWND) -> RECT {
    let mut rc = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
        rc.bottom = (rc.bottom - BAR * dpi / 96).max(rc.top + 10);
    }
    rc
}

fn layout(hwnd: HWND) {
    let rc = content_rect(hwnd);
    unsafe {
        let content = HWND(CONTENT.load(Ordering::Relaxed) as _);
        let _ = MoveWindow(content, 0, 0, rc.right, rc.bottom, true);
        let edit = HWND(EDIT.load(Ordering::Relaxed) as _);
        let _ = MoveWindow(edit, 0, 0, rc.right, rc.bottom, true);
        let inner = RECT { left: 0, top: 0, right: rc.right, bottom: rc.bottom };
        if let Some(Some(h)) = with(|v| v.handler.clone()) {
            let _ = h.SetRect(&inner);
        }
        if let Some(Some(p)) = with(|v| v.player.clone()) {
            let _ = p.UpdateVideo();
        }
    }
}

fn ext_of(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

fn human(n: u64) -> String {
    let units = ["bytes", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    let s = if i == 0 { format!("{n} {}", units[0]) } else { format!("{v:.1} {}", units[i]) };
    if crate::i18n::is_english() { s } else { s.replace('.', ",") }
}

/// Carrega o item atual.
fn load() {
    let Some((hwnd, path, i, n)) = with(|v| {
        if let Some(h) = v.handler.take() {
            unsafe {
                let _ = h.Unload();
            }
        }
        if let Some(p) = v.player.take() {
            unsafe {
                let _ = p.Shutdown();
            }
        }
        v.image = None;
        v.caption.clear();
        v.paused = false;
        v.list.get(v.index).map(|p| (v.hwnd, p.clone(), v.index, v.list.len()))
    })
    .flatten() else {
        return;
    };
    let edit = HWND(EDIT.load(Ordering::Relaxed) as _);
    let content = HWND(CONTENT.load(Ordering::Relaxed) as _);
    unsafe {
        let _ = ShowWindow(edit, SW_HIDE);
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string_lossy().into_owned());
    let title = if n > 1 { format!("{name}  ·  {} de {n}", i + 1) } else { name.clone() };
    unsafe {
        let _ = SetWindowTextW(hwnd, &HSTRING::from(title));
    }
    let meta = std::fs::metadata(&path).ok();
    let size = meta.as_ref().filter(|m| m.is_file()).map(|m| human(m.len())).unwrap_or_default();
    let ext = ext_of(&path);
    let area = content_rect(hwnd);

    let shown: &str = if path.is_dir() {
        show_text(edit, &folder_text(&path));
        "Pasta"
    } else if IMAGES.contains(&ext.as_str()) && decode_image(&path, area.right, area.bottom).is_some_and(|img| {
        with(|v| v.image = Some(img));
        true
    }) {
        "Imagem"
    } else if ext == "zip" && let Some(t) = zip_text(&path) {
        show_text(edit, &t);
        "Arquivo compactado"
    } else if MEDIA.contains(&ext.as_str()) && play(content, &path, AUDIO.contains(&ext.as_str())) {
        if AUDIO.contains(&ext.as_str()) {
            thumbnail(&path, area);
            "Áudio"
        } else {
            "Vídeo · clique para pausar"
        }
    } else if (TEXT.contains(&ext.as_str()) || looks_like_text(&path)) && let Some(t) = read_text(&path) {
        show_text(edit, &t);
        "Texto"
    } else if preview_handler(content, &path, area) {
        "Documento"
    } else {
        thumbnail(&path, area);
        "Arquivo"
    };
    let modified = meta
        .and_then(|m| m.modified().ok())
        .map(|t| {
            let secs = t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            crate::peek::date(secs)
        })
        .unwrap_or_default();
    let mut info = crate::i18n::tr(shown);
    for part in [size, modified] {
        if !part.is_empty() {
            info.push_str("  ·  ");
            info.push_str(&part);
        }
    }
    with(|v| v.info = info);
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, true);
        let _ = InvalidateRect(Some(content), None, true);
        let _ = SetFocus(Some(hwnd));
    }
}

/// Data local (dd/mm/aaaa hh:mm) a partir de segundos Unix.
pub fn date(secs: u64) -> String {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    let ft = (secs + 11_644_473_600) * 10_000_000;
    let f = FILETIME { dwLowDateTime: ft as u32, dwHighDateTime: (ft >> 32) as u32 };
    let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    unsafe {
        if FileTimeToSystemTime(&f, &mut utc).is_err() || SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_err() {
            return String::new();
        }
    }
    format!("{:02}/{:02}/{} {:02}:{:02}", local.wDay, local.wMonth, local.wYear, local.wHour, local.wMinute)
}

fn show_text(edit: HWND, text: &str) {
    let t: String = text.replace("\r\n", "\n").replace('\n', "\r\n");
    unsafe {
        let _ = SetWindowTextW(edit, &HSTRING::from(t));
        let _ = ShowWindow(edit, SW_SHOW);
    }
}

fn looks_like_text(p: &Path) -> bool {
    let Ok(mut f) = std::fs::File::open(p) else { return false };
    let mut buf = [0u8; 4096];
    use std::io::Read;
    let n = f.read(&mut buf).unwrap_or(0);
    n > 0 && !buf[..n].contains(&0) && std::str::from_utf8(&buf[..n.saturating_sub(4)]).is_ok()
}

fn read_text(p: &Path) -> Option<String> {
    use std::io::Read;
    let limit = (crate::extras::num("ql_text_kb") as u64) * 1024;
    let mut bytes = Vec::new();
    std::fs::File::open(p).ok()?.take(limit).read_to_end(&mut bytes).ok()?;
    let cut = std::fs::metadata(p).map(|m| m.len() > limit).unwrap_or(false);
    let mut s = if bytes.starts_with(&[0xFF, 0xFE]) {
        let u: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&u)
    } else {
        let b = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
        match std::str::from_utf8(b) {
            Ok(s) => s.to_string(),
            // Não é UTF-8: lê como Latin-1/Windows-1252 (texto antigo do Windows).
            Err(e) if e.valid_up_to() + 4 >= b.len() => String::from_utf8_lossy(b).into_owned(),
            Err(_) => b.iter().map(|&c| c as char).collect(),
        }
    };
    if cut {
        s.push_str(&format!("\n\n… ({})", crate::i18n::tr("arquivo cortado; Enter abre inteiro")));
    }
    Some(s)
}

fn folder_text(p: &Path) -> String {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut total = 0u64;
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            match e.metadata() {
                Ok(m) if m.is_dir() => dirs.push(name),
                Ok(m) => {
                    total += m.len();
                    files.push((name, m.len()));
                }
                _ => {}
            }
        }
    }
    dirs.sort_by_key(|d| d.to_lowercase());
    files.sort_by_key(|f| f.0.to_lowercase());
    let tr = crate::i18n::tr;
    let mut out = format!(
        "{}\n\n{}: {}    {}: {}    {}: {}\n\n",
        p.to_string_lossy(),
        tr("Pastas"),
        dirs.len(),
        tr("Arquivos"),
        files.len(),
        tr("Tamanho dos arquivos"),
        human(total)
    );
    for d in dirs.iter().take(500) {
        out.push_str(&format!("[{d}]\n"));
    }
    for (f, s) in files.iter().take(1500) {
        out.push_str(&format!("{f}    {}\n", human(*s)));
    }
    out
}

/// Lista do .zip lida do diretório central (sem extrair nada).
fn zip_text(p: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(p).ok()?;
    let len = f.metadata().ok()?.len();
    let tail = len.min(66_000);
    f.seek(SeekFrom::Start(len - tail)).ok()?;
    let mut buf = vec![0u8; tail as usize];
    f.read_exact(&mut buf).ok()?;
    let eocd = (0..buf.len().saturating_sub(21)).rev().find(|&i| buf[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])?;
    let rd16 = |b: &[u8], o: usize| u16::from_le_bytes([b[o], b[o + 1]]) as usize;
    let rd32 = |b: &[u8], o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as u64;
    let count = rd16(&buf, eocd + 10);
    let size = rd32(&buf, eocd + 12);
    let offset = rd32(&buf, eocd + 16);
    if size > 64 * 1024 * 1024 {
        return None;
    }
    f.seek(SeekFrom::Start(offset)).ok()?;
    let mut cd = vec![0u8; size as usize];
    f.read_exact(&mut cd).ok()?;
    let mut out = format!("{}: {count}\n\n", crate::i18n::tr("Itens no arquivo"));
    let mut o = 0;
    for _ in 0..count.min(3000) {
        if o + 46 > cd.len() || cd[o..o + 4] != [0x50, 0x4b, 0x01, 0x02] {
            break;
        }
        let usize_ = rd32(&cd, o + 24);
        let (n, x, c) = (rd16(&cd, o + 28), rd16(&cd, o + 30), rd16(&cd, o + 32));
        let name = String::from_utf8_lossy(cd.get(o + 46..o + 46 + n)?).into_owned();
        if name.ends_with('/') {
            out.push_str(&format!("[{name}]\n"));
        } else {
            out.push_str(&format!("{name}    {}\n", human(usize_)));
        }
        o += 46 + n + x + c;
    }
    Some(out)
}

/// Decodifica a imagem já reduzida para caber em `mw` × `mh` e na orientação da câmera.
fn decode_image(p: &Path, mw: i32, mh: i32) -> Option<(Vec<u8>, i32, i32)> {
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let dec = wic.CreateDecoderFromFilename(&HSTRING::from(p.as_os_str()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = dec.GetFrame(0).ok()?;
        // Orientação EXIF (1 = normal; 3 = 180°; 6 = 90° horário; 8 = 90° anti-horário).
        let orient = frame
            .GetMetadataQueryReader()
            .ok()
            .and_then(|q| {
                let mut pv = windows::Win32::System::Com::StructuredStorage::PROPVARIANT::default();
                q.GetMetadataByName(w!("/app1/ifd/{ushort=274}"), &mut pv).ok()?;
                u16::try_from(&pv).ok()
            })
            .unwrap_or(1);
        let src: IWICBitmapSource = match orient {
            3 | 6 | 8 => {
                let rot = wic.CreateBitmapFlipRotator().ok()?;
                let o = match orient {
                    3 => WICBitmapTransformRotate180,
                    6 => WICBitmapTransformRotate90,
                    _ => WICBitmapTransformRotate270,
                };
                rot.Initialize(&frame, o).ok()?;
                rot.cast().ok()?
            }
            _ => frame.cast().ok()?,
        };
        let (mut w, mut h) = (0u32, 0u32);
        src.GetSize(&mut w, &mut h).ok()?;
        let k = (mw.max(1) as f32 / w as f32).min(mh.max(1) as f32 / h as f32).min(1.0);
        let (tw, th) = (((w as f32 * k) as u32).max(1), ((h as f32 * k) as u32).max(1));
        let scaler = wic.CreateBitmapScaler().ok()?;
        scaler.Initialize(&src, tw, th, WICBitmapInterpolationModeFant).ok()?;
        let conv = wic.CreateFormatConverter().ok()?;
        conv.Initialize(&scaler, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        let mut buf = vec![0u8; (tw * th * 4) as usize];
        conv.CopyPixels(std::ptr::null(), tw * 4, &mut buf).ok()?;
        Some((buf, tw as i32, th as i32))
    }
}

/// Miniatura do Windows (vídeo, PDF, qualquer tipo) com o nome embaixo.
fn thumbnail(p: &Path, area: RECT) {
    let side = (area.right.min(area.bottom) / 2).clamp(96, 512);
    let img = unsafe {
        (|| -> Option<(Vec<u8>, i32, i32)> {
            let f: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(p.as_os_str()), None).ok()?;
            let hb = f.GetImage(windows::Win32::Foundation::SIZE { cx: side, cy: side }, SIIGBF_BIGGERSIZEOK).ok()?;
            let mut bm = BITMAP::default();
            GetObjectW(HGDIOBJ(hb.0), size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _));
            let (w, h) = (bm.bmWidth, bm.bmHeight);
            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
                ..Default::default()
            };
            let mut buf = vec![0u8; (w * h * 4) as usize];
            let dc = GetDC(None);
            GetDIBits(dc, hb, 0, h as u32, Some(buf.as_mut_ptr() as *mut _), &mut bmi, DIB_RGB_COLORS);
            ReleaseDC(None, dc);
            let _ = DeleteObject(HGDIOBJ(hb.0));
            Some((buf, w, h))
        })()
    };
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    with(|v| {
        v.image = img;
        v.caption = name;
    });
}

fn play(content: HWND, p: &Path, audio: bool) -> bool {
    unsafe {
        let mut player: Option<IMFPMediaPlayer> = None;
        let target = if audio { None } else { Some(content) };
        if MFPCreateMediaPlayer(&HSTRING::from(p.as_os_str()), true, windows::Win32::Media::MediaFoundation::MFP_CREATION_OPTIONS(0), None, target, Some(&mut player)).is_err() {
            return false;
        }
        let Some(player) = player else { return false };
        with(|v| v.player = Some(player));
        true
    }
}

/// CLSID do pré-visualizador registrado para a extensão (o mesmo do Explorador).
fn handler_clsid(ext: &str) -> Option<GUID> {
    const KEY: &str = "ShellEx\\{8895b1c6-b41f-4c1c-a562-0d564250836f}";
    let read = |sub: String| -> Option<String> {
        let mut buf = [0u16; 260];
        let mut len = (buf.len() * 2) as u32;
        unsafe {
            RegGetValueW(HKEY_CLASSES_ROOT, &HSTRING::from(sub), None, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut len))
                .ok()
                .ok()?;
        }
        let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
        Some(String::from_utf16_lossy(&buf[..n]))
    };
    let dot = format!(".{ext}");
    let s = read(format!("{dot}\\{KEY}"))
        .or_else(|| read(format!("SystemFileAssociations\\{dot}\\{KEY}")))
        .or_else(|| read(dot.clone()).and_then(|progid| read(format!("{progid}\\{KEY}"))))?;
    GUID::try_from(s.trim_matches(|c| c == '{' || c == '}')).ok()
}

fn preview_handler(content: HWND, p: &Path, area: RECT) -> bool {
    let Some(clsid) = handler_clsid(&ext_of(p)) else { return false };
    unsafe {
        let h: IPreviewHandler = match CoCreateInstance(&clsid, None, CLSCTX_LOCAL_SERVER)
            .or_else(|_| CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER))
        {
            Ok(h) => h,
            Err(_) => return false,
        };
        let path = HSTRING::from(p.as_os_str());
        let ok = if let Ok(i) = h.cast::<IInitializeWithItem>() {
            SHCreateItemFromParsingName::<_, _, IShellItem>(&path, None).and_then(|it| i.Initialize(&it, STGM_READ.0)).is_ok()
        } else if let Ok(i) = h.cast::<IInitializeWithFile>() {
            i.Initialize(&path, STGM_READ.0).is_ok()
        } else if let Ok(i) = h.cast::<IInitializeWithStream>() {
            SHCreateStreamOnFileEx(&path, STGM_READ.0, 0, false, None).and_then(|s| i.Initialize(&s, STGM_READ.0)).is_ok()
        } else {
            false
        };
        if !ok {
            return false;
        }
        let r = RECT { left: 0, top: 0, right: area.right, bottom: area.bottom };
        if h.SetWindow(content, &r).is_err() || h.DoPreview().is_err() {
            let _ = h.Unload();
            return false;
        }
        with(|v| v.handler = Some(h));
        true
    }
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_SIZE => {
                layout(hwnd);
                let _ = InvalidateRect(Some(hwnd), None, true);
                LRESULT(0)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut ps);
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                let top = content_rect(hwnd).bottom;
                let bar = RECT { top, ..rc };
                let br = CreateSolidBrush(COLORREF(0x00241F1F));
                FillRect(dc, &bar, br);
                let _ = DeleteObject(HGDIOBJ(br.0));
                if let Some((info, font)) = with(|v| (v.info.clone(), v.ui_font)) {
                    let old = SelectObject(dc, HGDIOBJ(font.0));
                    SetBkMode(dc, TRANSPARENT);
                    SetTextColor(dc, COLORREF(0x00B4B0AE));
                    let mut t: Vec<u16> = info.encode_utf16().collect();
                    let mut r = RECT { left: bar.left + 14, right: bar.right - 14, ..bar };
                    DrawTextW(dc, &mut t, &mut r, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
                    SelectObject(dc, old);
                }
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ACTIVATE if (wp.0 & 0xffff) != 0 => {
                let _ = SetFocus(Some(hwnd));
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

unsafe extern "system" fn content_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT => {
                let dc = HDC(wp.0 as _);
                SetTextColor(dc, COLORREF(0x00E6E2E0));
                SetBkColor(dc, COLORREF(BG));
                LRESULT(EDIT_BRUSH.load(Ordering::Relaxed))
            }
            WM_LBUTTONUP => {
                // Vídeo: clique pausa / continua.
                with(|v| {
                    if let Some(p) = &v.player {
                        let _ = if v.paused { p.Play() } else { p.Pause() };
                        v.paused = !v.paused;
                    }
                });
                LRESULT(0)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut ps);
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                with(|v| {
                    if let Some(p) = &v.player {
                        let _ = p.UpdateVideo();
                    }
                    let Some((buf, w, h)) = &v.image else { return };
                    let caption = !v.caption.is_empty();
                    let avail_h = if caption { rc.bottom - 60 } else { rc.bottom };
                    let (x, y) = ((rc.right - w) / 2, ((avail_h - h) / 2).max(0));
                    let bmi = BITMAPINFO {
                        bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: *w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
                        ..Default::default()
                    };
                    // Alfa por cima do fundo escuro (pixels já pré-multiplicados).
                    let bg = [(BG & 0xff) as u8, ((BG >> 8) & 0xff) as u8, ((BG >> 16) & 0xff) as u8];
                    let mut px = buf.clone();
                    for c in px.chunks_exact_mut(4) {
                        let a = 255 - c[3] as u32;
                        if a > 0 {
                            c[0] = (c[0] as u32 + bg[2] as u32 * a / 255) as u8;
                            c[1] = (c[1] as u32 + bg[1] as u32 * a / 255) as u8;
                            c[2] = (c[2] as u32 + bg[0] as u32 * a / 255) as u8;
                        }
                    }
                    StretchDIBits(dc, x, y, *w, *h, 0, 0, *w, *h, Some(px.as_ptr() as *const _), &bmi, DIB_RGB_COLORS, SRCCOPY);
                    if caption {
                        let old = SelectObject(dc, HGDIOBJ(v.ui_font.0));
                        SetBkMode(dc, TRANSPARENT);
                        SetTextColor(dc, COLORREF(0x00E6E2E0));
                        let mut t: Vec<u16> = v.caption.encode_utf16().collect();
                        let mut r = RECT { left: 10, top: y + h + 12, right: rc.right - 10, bottom: y + h + 44 };
                        DrawTextW(dc, &mut t, &mut r, DT_CENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
                        SelectObject(dc, old);
                    }
                });
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
