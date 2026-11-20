//! Menu de energia (último item da topbar): bloquear, suspender, sair, reiniciar e desligar.

use std::os::windows::process::CommandExt;

use windows::Win32::System::Power::SetSuspendState;
use windows::Win32::System::Shutdown::LockWorkStation;

use crate::panel::{self, Event, PanelContent, Widget};

const LOCK: u32 = 1;
const SLEEP: u32 = 2;
const SIGN_OUT: u32 = 3;
const RESTART: u32 = 4;
const SHUTDOWN: u32 = 5;

pub struct PowerPanel;

/// `shutdown.exe` sem janela de console. Sem /f: apps com trabalho não salvo ainda podem pedir
/// para salvar.
fn shutdown(args: &[&str]) {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = std::process::Command::new("shutdown").args(args).creation_flags(CREATE_NO_WINDOW).spawn();
}

impl PanelContent for PowerPanel {
    fn width(&self) -> f32 {
        220.0
    }
    fn build(&mut self) -> Vec<Widget> {
        let row = |id, glyph: u16, text: &str| Widget::Row { id, glyph: Some(glyph), text: text.into(), sub: None, selected: false };
        vec![
            row(LOCK, 0xE72E, "Bloquear"),
            row(SLEEP, 0xE708, "Suspender"),
            row(SIGN_OUT, 0xF3B1, "Sair"),
            Widget::Separator,
            row(RESTART, 0xE777, "Reiniciar"),
            row(SHUTDOWN, 0xE7E8, "Desligar"),
        ]
    }
    fn event(&mut self, ev: Event) -> bool {
        let Event::Click(id) = ev else { return false };
        panel::close();
        unsafe {
            match id {
                LOCK => {
                    let _ = LockWorkStation();
                }
                SLEEP => {
                    SetSuspendState(false, false, false);
                }
                SIGN_OUT => shutdown(&["/l"]),
                RESTART => shutdown(&["/r", "/t", "0"]),
                SHUTDOWN => shutdown(&["/s", "/t", "0"]),
                _ => {}
            }
        }
        false
    }
}
