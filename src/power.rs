//! Menu de energia: bloquear, suspender, sair, reiniciar e desligar. Abre pelo módulo
//! "Bateria e energia" da topbar (com a bateria no topo) e pela Central de Controle.

use std::os::windows::process::CommandExt;

use windows::Win32::System::Power::SetSuspendState;
use windows::Win32::System::Shutdown::LockWorkStation;

use crate::panel::{self, Event, PanelContent, Widget};

pub const LOCK: u32 = 1;
pub const SLEEP: u32 = 2;
pub const SIGN_OUT: u32 = 3;
pub const RESTART: u32 = 4;
pub const SHUTDOWN: u32 = 5;
/// Linha da bateria: abre as opções de energia do Windows.
const SETTINGS: u32 = 6;

pub struct PowerPanel {
    /// Linha da bateria no topo (o módulo da topbar; a Central já mostra a bateria no cartão).
    pub battery: bool,
}

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
        let mut out = Vec::new();
        if self.battery
            && let Some(b) = crate::status::battery()
        {
            let sub = if b.charging { "Conectado na tomada" } else { "Usando a bateria" };
            out.push(Widget::Row {
                id: SETTINGS,
                glyph: Some(crate::status::battery_glyph(&b)),
                text: format!("Bateria {}%", b.percent),
                sub: Some(sub.into()),
                selected: false,
            });
            out.push(Widget::Separator);
        }
        out.extend([
            row(LOCK, 0xE72E, "Bloquear"),
            row(SLEEP, 0xE708, "Suspender"),
            row(SIGN_OUT, 0xF3B1, "Sair"),
            Widget::Separator,
            row(RESTART, 0xE777, "Reiniciar"),
            row(SHUTDOWN, 0xE7E8, "Desligar"),
        ]);
        out
    }
    fn event(&mut self, ev: Event) -> bool {
        let Event::Click(id) = ev else { return false };
        panel::close();
        run(id);
        false
    }
}

/// Executa a ação `id` ([`LOCK`], [`SLEEP`], [`SIGN_OUT`], [`RESTART`] ou [`SHUTDOWN`]).
pub fn run(id: u32) {
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
            SETTINGS => {
                use windows::Win32::UI::Shell::ShellExecuteW;
                use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
                ShellExecuteW(None, windows::core::w!("open"), windows::core::w!("ms-settings:powersleep"), None, None, SW_SHOWNORMAL);
            }
            _ => {}
        }
    }
}
