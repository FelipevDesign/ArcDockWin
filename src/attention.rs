//! Ícone pulando (estilo macOS): quando um app pede atenção (FlashWindow, que chega ao shell
//! como HSHELL_FLASH), o ícone dele pula algumas vezes e fica com um pontinho vermelho até o
//! app vir para o primeiro plano.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, RegisterShellHookWindow, RegisterWindowMessageW,
};
use windows::core::w;

const HSHELL_WINDOWDESTROYED: usize = 2;
const HSHELL_WINDOWACTIVATED: usize = 4;
const HSHELL_RUDEAPPACTIVATED: usize = 0x8004;
const HSHELL_FLASH: usize = 0x8006;

/// Quantos pulos e quanto dura cada um.
const BOUNCES: f32 = 3.0;
const BOUNCE_TIME: f32 = 0.6;

static SHELLHOOK: AtomicU32 = AtomicU32::new(0);

thread_local! {
    /// Janelas pedindo atenção (HWND como número) e quando começaram.
    static FLASHING: RefCell<HashMap<isize, Instant>> = RefCell::new(HashMap::new());
}

/// Passa a receber as mensagens do shell (as mesmas que a barra do Windows recebe).
pub fn register(hwnd: HWND) {
    unsafe {
        SHELLHOOK.store(RegisterWindowMessageW(w!("SHELLHOOK")), Ordering::Relaxed);
        let _ = RegisterShellHookWindow(hwnd);
    }
}

pub fn is_hook(msg: u32) -> bool {
    msg != 0 && msg == SHELLHOOK.load(Ordering::Relaxed)
}

/// Trata uma mensagem do shell; true se algo mudou (precisa redesenhar).
pub fn handle(wp: WPARAM, lp: LPARAM) -> bool {
    let win = lp.0;
    FLASHING.with_borrow_mut(|f| match wp.0 {
        // Pisca de novo enquanto já está pulando: não recomeça.
        HSHELL_FLASH if !f.contains_key(&win) => {
            if unsafe { GetForegroundWindow() }.0 as isize == win {
                return false;
            }
            f.insert(win, Instant::now());
            true
        }
        HSHELL_WINDOWACTIVATED | HSHELL_RUDEAPPACTIVATED | HSHELL_WINDOWDESTROYED => {
            f.remove(&win).is_some()
        }
        _ => false,
    })
}

/// Algum ícone ainda pulando (mantém o laço de animação ligado).
pub fn bouncing() -> bool {
    FLASHING.with_borrow(|f| f.values().any(|s| s.elapsed().as_secs_f32() < BOUNCES * BOUNCE_TIME))
}

/// Altura do pulo (0..1) e se mostra o pontinho vermelho, para o app com estas janelas.
/// Se o app já está em primeiro plano, o pedido de atenção acaba.
pub fn state(windows: &[HWND]) -> (f32, bool) {
    if windows.is_empty() {
        return (0.0, false);
    }
    FLASHING.with_borrow_mut(|f| {
        if f.is_empty() {
            return (0.0, false);
        }
        let fg = unsafe { GetForegroundWindow() };
        if windows.contains(&fg) {
            for w in windows {
                f.remove(&(w.0 as isize));
            }
            return (0.0, false);
        }
        let Some(start) = windows.iter().filter_map(|w| f.get(&(w.0 as isize))).min() else {
            return (0.0, false);
        };
        let t = start.elapsed().as_secs_f32();
        if t >= BOUNCES * BOUNCE_TIME {
            return (0.0, true);
        }
        // Parábola: sobe e desce em cada pulo.
        let p = (t % BOUNCE_TIME) / BOUNCE_TIME;
        (4.0 * p * (1.0 - p), true)
    })
}
