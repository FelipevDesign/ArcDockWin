//! Rede de segurança: um processo vigia (o mesmo exe com `--guard <pid>`) espera o dock
//! terminar. Se ele morrer sem restaurar a barra do Windows (travou, foi encerrado à força),
//! o vigia restaura a barra e avisa os apps para reenviarem os ícones da bandeja ao Explorer.

use windows::Win32::Foundation::{CloseHandle, LPARAM, WPARAM};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, HWND_BROADCAST, RegisterWindowMessageW, SendNotifyMessageW,
};
use windows::core::w;

/// Abre o vigia para este processo. Falhar aqui não impede o dock de rodar.
pub fn spawn() {
    let Ok(exe) = std::env::current_exe() else { return };
    let pid = unsafe { GetCurrentProcessId() };
    let _ = std::process::Command::new(exe).arg("--guard").arg(pid.to_string()).spawn();
}

/// Corpo do vigia. Devolve `true` se o processo foi aberto como vigia.
pub fn run_if_requested() -> bool {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("--guard") {
        return false;
    }
    let Some(pid) = args.next().and_then(|p| p.parse::<u32>().ok()) else { return true };
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else { return true };
        WaitForSingleObject(process, INFINITE);
        let _ = CloseHandle(process);
    }
    // Outro dock já aberto cuida da barra e da animação.
    if unsafe { FindWindowW(w!("ArcDockWin"), None) }.is_ok() {
        return true;
    }
    // Animação nativa de minimizar desligada no meio de uma animação nossa (minimize.rs).
    crate::minimize::restore_native();
    // Saída normal já restaurou (e apagou o arquivo de estado).
    if !crate::taskbar::needs_restore() {
        return true;
    }
    crate::taskbar::restore();
    unsafe {
        let msg = RegisterWindowMessageW(w!("TaskbarCreated"));
        let _ = SendNotifyMessageW(HWND_BROADCAST, msg, WPARAM(0), LPARAM(0));
    }
    true
}
