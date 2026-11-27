//! Progresso nos ícones (como o verde da barra de tarefas): os apps mandam o progresso pela
//! ITaskbarList3, e o Windows entrega isso à janela registrada na propriedade `TaskbandHWND`
//! da barra de tarefas. Com a barra escondida pelo dock essa propriedade fica vazia; o dock se
//! registra nela (só se estiver vazia ou apontando para uma janela que não existe mais) e
//! recebe `WM_USER+65` (estado: hwnd, flags) e `WM_USER+64` (valor: hwnd, 0..65535).
//! Desligado, ou ao sair, a propriedade volta a ficar vazia.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};

use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

const MSG_VALUE: u32 = WM_USER + 64;
const MSG_STATE: u32 = WM_USER + 65;
pub const NORMAL: u32 = 2;
pub const INDETERMINATE: u32 = 1;
pub const ERROR: u32 = 4;
pub const PAUSED: u32 = 8;

static WINDOW: AtomicIsize = AtomicIsize::new(0);
static TIMER: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// Janela → (estado, fração 0..1).
    static STATE: RefCell<HashMap<isize, (u32, f32)>> = RefCell::new(HashMap::new());
}

fn tray() -> HWND {
    unsafe { FindWindowW(w!("Shell_TrayWnd"), None).unwrap_or_default() }
}

/// Registra o dock como destino do progresso (se ninguém mais for).
fn claim() {
    let me = WINDOW.load(Ordering::Relaxed);
    let t = tray();
    if me == 0 || t.is_invalid() {
        return;
    }
    unsafe {
        let cur = GetPropW(t, w!("TaskbandHWND"));
        let free = cur.is_invalid() || !IsWindow(Some(HWND(cur.0))).as_bool();
        if free && cur.0 as isize != me {
            let _ = SetPropW(t, w!("TaskbandHWND"), Some(HANDLE(me as _)));
        }
    }
}

fn release() {
    let me = WINDOW.load(Ordering::Relaxed);
    let t = tray();
    if me == 0 || t.is_invalid() {
        return;
    }
    unsafe {
        if GetPropW(t, w!("TaskbandHWND")).0 as isize == me {
            let _ = RemovePropW(t, w!("TaskbandHWND"));
        }
    }
}

pub fn sync() {
    let want = crate::extras::on("fx_progress");
    let cur = WINDOW.load(Ordering::Relaxed);
    unsafe {
        if want && cur == 0 {
            let inst = GetModuleHandleW(None).unwrap_or_default();
            let wc = WNDCLASSW { lpfnWndProc: Some(proc_), hInstance: inst.into(), lpszClassName: w!("ArcDockProgress"), ..Default::default() };
            RegisterClassW(&wc);
            let Ok(h) = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("ArcDockProgress"),
                w!(""),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(inst.into()),
                None,
            ) else {
                return;
            };
            // Apps com permissão de administrador também conseguem mandar o progresso.
            for m in [MSG_VALUE, MSG_STATE] {
                let _ = ChangeWindowMessageFilterEx(h, m, MSGFLT_ALLOW, None);
            }
            WINDOW.store(h.0 as isize, Ordering::Relaxed);
            claim();
            // O Explorador reinicia de vez em quando (e cria outra barra): confere de novo.
            let id = SetTimer(Some(h), 1, 10_000, None);
            TIMER.store(id, Ordering::Relaxed);
        } else if !want && cur != 0 {
            release();
            let _ = DestroyWindow(HWND(cur as _));
            WINDOW.store(0, Ordering::Relaxed);
            STATE.with_borrow_mut(|s| s.clear());
            crate::apply_settings(false);
        }
    }
}

/// Ao sair do dock: devolve a propriedade.
pub fn shutdown() {
    release();
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let target = wp.0 as isize;
    let changed = match msg {
        MSG_STATE => STATE.with_borrow_mut(|s| {
            let flags = lp.0 as u32;
            if flags == 0 {
                s.remove(&target).is_some()
            } else {
                let e = s.entry(target).or_insert((flags, 0.0));
                let old = e.0;
                e.0 = flags;
                old != flags
            }
        }),
        MSG_VALUE => STATE.with_borrow_mut(|s| {
            let v = (lp.0 as u32 as f32 / 65535.0).clamp(0.0, 1.0);
            let e = s.entry(target).or_insert((NORMAL, 0.0));
            // Valor novo sem estado: a barra normal (como na barra de tarefas).
            if e.0 == INDETERMINATE {
                e.0 = NORMAL;
            }
            let moved = (e.1 - v).abs() >= 0.005;
            e.1 = v;
            moved
        }),
        WM_TIMER => {
            claim();
            // Janelas que fecharam sem limpar o progresso.
            STATE.with_borrow_mut(|s| s.retain(|h, _| unsafe { IsWindow(Some(HWND(*h as _))).as_bool() }));
            false
        }
        _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    };
    if changed {
        crate::refresh_progress();
    }
    LRESULT(0)
}

/// Progresso de um app (o da janela mais atrasada, como a barra de tarefas faz).
pub fn of(windows: &[HWND]) -> Option<(u32, f32)> {
    if WINDOW.load(Ordering::Relaxed) == 0 || windows.is_empty() {
        return None;
    }
    STATE.with(|s| {
        let s = s.try_borrow().ok()?;
        let all: Vec<(u32, f32)> = windows.iter().filter_map(|h| s.get(&(h.0 as isize)).copied()).collect();
        // Prioridade: erro, pausado, normal (menor valor), indeterminado.
        for f in [ERROR, PAUSED, NORMAL] {
            if let Some(p) = all.iter().filter(|p| p.0 & f != 0).min_by(|a, b| a.1.total_cmp(&b.1)) {
                return Some((f, p.1));
            }
        }
        all.iter().find(|p| p.0 & INDETERMINATE != 0).map(|_| (INDETERMINATE, 0.0))
    })
}
