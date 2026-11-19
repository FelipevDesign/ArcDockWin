//! Esconde a barra de tarefas do Windows enquanto o dock roda e restaura ao sair.

use std::path::PathBuf;

use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::Shell::{ABM_GETSTATE, ABM_SETSTATE, ABS_AUTOHIDE, APPBARDATA, SHAppBarMessage};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetSystemMetrics, GetWindowThreadProcessId, IsWindowVisible, SM_CXSCREEN,
    SM_CYSCREEN, SW_HIDE, SW_SHOW, ShowWindow,
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
