//! Rede de segurança: um processo vigia (o mesmo exe com `--guard <pid>`) espera o dock
//! terminar. Se ele morrer sem restaurar a barra do Windows (travou, foi encerrado à força),
//! o vigia restaura a barra e avisa os apps para reenviarem os ícones da bandeja ao Explorer.
//! Depois de um travamento, o vigia abre o dock de novo se ele tinha rodado pelo menos
//! `MIN_UPTIME` (um dock que trava ao abrir não fica reabrindo sem parar).

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, GetExitCodeProcess, INFINITE, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, HWND_BROADCAST, RegisterWindowMessageW, SendNotifyMessageW,
};
use windows::core::w;

/// Tempo mínimo de vida do dock para o vigia reabri-lo depois de um travamento.
const MIN_UPTIME: Duration = Duration::from_secs(60);

fn should_restart(exit_code: Option<u32>, uptime: Duration) -> bool {
    exit_code.is_some_and(|code| code != 0) && uptime >= MIN_UPTIME
}

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
    let started = Instant::now();
    let exit_code = unsafe {
        let access = PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION;
        let Ok(process) = OpenProcess(access, false, pid) else { return true };
        let waited = WaitForSingleObject(process, INFINITE);
        let mut code = 0;
        let exit_code = if waited == WAIT_OBJECT_0 {
            GetExitCodeProcess(process, &mut code).ok().map(|()| code)
        } else {
            None
        };
        let _ = CloseHandle(process);
        if waited != WAIT_OBJECT_0 {
            return true;
        }
        exit_code
    };
    let uptime = started.elapsed();
    // Outro dock já aberto cuida da barra e da animação.
    if unsafe { FindWindowW(w!("ArcDockWin"), None) }.is_ok() {
        return true;
    }
    // Animação nativa de minimizar desligada no meio de uma animação nossa (minimize.rs).
    crate::minimize::restore_native();
    if let Some(code) = exit_code.filter(|&code| code != 0) {
        crate::log::line(&format!(
            "o dock {pid} terminou com código {code:#010X} após {} s",
            uptime.as_secs()
        ));
    }
    if crate::taskbar::needs_restore() {
        crate::log::line("restaurando a barra do Windows");
        crate::taskbar::restore();
        unsafe {
            let msg = RegisterWindowMessageW(w!("TaskbarCreated"));
            let _ = SendNotifyMessageW(HWND_BROADCAST, msg, WPARAM(0), LPARAM(0));
        }
    }
    if should_restart(exit_code, uptime)
        && let Ok(exe) = std::env::current_exe()
        && std::process::Command::new(exe).spawn().is_ok()
    {
        crate::log::line("dock reaberto pelo vigia");
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_requires_a_failure_and_minimum_uptime() {
        assert!(!should_restart(Some(0), MIN_UPTIME));
        assert!(!should_restart(None, MIN_UPTIME));
        assert!(!should_restart(Some(1), MIN_UPTIME - Duration::from_millis(1)));
        assert!(should_restart(Some(0xC000000D), MIN_UPTIME));
    }
}
