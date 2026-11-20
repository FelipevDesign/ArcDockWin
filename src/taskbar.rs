//! Esconde a barra de tarefas do Windows enquanto o dock roda e restaura ao sair.

use std::path::PathBuf;

use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::Shell::{ABM_GETSTATE, ABM_SETSTATE, ABS_AUTOHIDE, APPBARDATA, SHAppBarMessage};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetSystemMetrics, GetWindowThreadProcessId, IsWindowVisible, SM_CXSCREEN,
    SM_CYSCREEN, SW_HIDE, SW_SHOW, ShowWindow,
};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows::Win32::UI::WindowsAndMessaging::{
    CHILDID_SELF, EVENT_OBJECT_SHOW, GetClassNameW, OBJID_WINDOW, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS,
};
use windows::core::w;

fn state_file() -> PathBuf {
    crate::apps::config_path().with_file_name("taskbar_state")
}

/// Shell_TrayWnd do Explorer. Não usa FindWindow direto porque o host da bandeja
/// (tray.rs) cria uma janela nossa com a mesma classe, acima da do Explorer.
pub fn explorer_tray() -> Option<HWND> {
    let own = unsafe { GetCurrentProcessId() };
    let mut prev = None;
    while let Ok(h) = unsafe { FindWindowExW(None, prev, w!("Shell_TrayWnd"), None) } {
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
        if pid != own {
            return Some(h);
        }
        prev = Some(h);
    }
    None
}

fn tray_windows() -> Vec<HWND> {
    let mut out = Vec::new();
    unsafe {
        if let Some(main) = explorer_tray() {
            out.push(main);
        }
        let mut prev = None;
        while let Ok(h) = FindWindowExW(None, prev, w!("Shell_SecondaryTrayWnd"), None) {
            out.push(h);
            prev = Some(h);
        }
    }
    out
}

fn appbar_data(hwnd: HWND) -> APPBARDATA {
    APPBARDATA { cbSize: size_of::<APPBARDATA>() as u32, hWnd: hwnd, ..Default::default() }
}

fn set_state(state: u32) {
    if let Some(tray) = explorer_tray() {
        let mut abd = appbar_data(tray);
        abd.lParam = LPARAM(state as isize);
        unsafe { SHAppBarMessage(ABM_SETSTATE, &mut abd) };
    }
}

/// Liga o ocultar automático e esconde as barras. Guarda o estado original em disco
/// (se o dock morrer sem restaurar, a próxima execução ainda sabe qual era).
pub fn hide() {
    let file = state_file();
    if std::fs::read_to_string(&file).ok().and_then(|s| s.trim().parse::<u32>().ok()).is_none() {
        let mut abd = appbar_data(HWND::default());
        let current = unsafe { SHAppBarMessage(ABM_GETSTATE, &mut abd) } as u32;
        let _ = std::fs::write(&file, current.to_string());
    }
    set_state(ABS_AUTOHIDE);
    ensure_hidden();
    watch_show();
}

/// O Explorer mostra a barra de novo em várias situações (print, Win+T, tela cheia...).
/// Em vez de esperar o timer, esconde no mesmo instante em que ela aparece.
fn watch_show() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static HOOKED: AtomicBool = AtomicBool::new(false);
    if HOOKED.swap(true, Ordering::Relaxed) {
        return;
    }
    unsafe {
        let _ = SetWinEventHook(
            EVENT_OBJECT_SHOW,
            EVENT_OBJECT_SHOW,
            None,
            Some(on_show),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
    }
}

unsafe extern "system" fn on_show(
    _: HWINEVENTHOOK,
    _: u32,
    hwnd: HWND,
    obj: i32,
    child: i32,
    _: u32,
    _: u32,
) {
    if hwnd.is_invalid() || obj != OBJID_WINDOW.0 || child != CHILDID_SELF as i32 {
        return;
    }
    let mut buf = [0u16; 32];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
    let class = &buf[..n];
    if class == "Shell_TrayWnd".encode_utf16().collect::<Vec<_>>()
        || class == "Shell_SecondaryTrayWnd".encode_utf16().collect::<Vec<_>>()
    {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

/// O Explorer às vezes mostra a barra de novo; chamado periodicamente.
pub fn ensure_hidden() {
    for h in tray_windows() {
        unsafe {
            if IsWindowVisible(h).as_bool() {
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
    }
}

/// O dock escondeu a barra e ainda não restaurou (o arquivo de estado só existe nesse intervalo).
pub fn needs_restore() -> bool {
    state_file().exists()
}

pub fn restore() {
    let file = state_file();
    let original = std::fs::read_to_string(&file)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);
    set_state(original);
    for h in tray_windows() {
        unsafe {
            let _ = ShowWindow(h, SW_SHOW);
        }
    }
    let _ = std::fs::remove_file(file);
}

/// Tamanho da tela principal.
pub fn screen_size() -> (i32, i32) {
    unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) }
}
