//! Diário curto de início e saída do dock em `%APPDATA%\ArcDockWin\arcdockwin.log`: quando o
//! dock some sozinho, diz quem pediu a saída (menu, WM_CLOSE de fora, fim de sessão...) ou se
//! ele travou. Passando de 64 KB, o arquivo recomeça.

use std::io::Write;

const MAX_BYTES: u64 = 64 * 1024;

fn path() -> std::path::PathBuf {
    crate::apps::config_path().with_file_name("arcdockwin.log")
}

/// Uma linha com data, hora e o PID deste processo.
pub fn line(text: &str) {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let pid = std::process::id();
    let entry = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} [{pid}] {text}\r\n",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    );
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let fresh = std::fs::metadata(&p).is_ok_and(|m| m.len() > MAX_BYTES);
    let file = std::fs::OpenOptions::new().create(true).append(!fresh).write(true).truncate(fresh).open(&p);
    if let Ok(mut f) = file {
        let _ = f.write_all(entry.as_bytes());
    }
}
