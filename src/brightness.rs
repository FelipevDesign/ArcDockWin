//! Brilho do monitor da barra clicada por DDC/CI (dxva2). As chamadas são lentas (dezenas de ms),
//! então tudo roda em outra thread: a leitura avisa a topbar com `control::WM_UPDATE` e a
//! escrita aplica só o valor mais recente enquanto o usuário arrasta.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU32, Ordering};

use windows::Win32::Devices::Display::{
    DestroyPhysicalMonitors, GetMonitorBrightness, GetNumberOfPhysicalMonitorsFromHMONITOR,
    GetPhysicalMonitorsFromHMONITOR, PHYSICAL_MONITOR, SetMonitorBrightness,
};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{HMONITOR, MONITOR_DEFAULTTONEAREST, MonitorFromPoint};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, PostMessageW};

/// Brilho atual em %; -1 = não lido ou sem suporte (o slider some).
static LEVEL: AtomicI32 = AtomicI32::new(-1);
/// Faixa do monitor: (mínimo, máximo).
static MIN: AtomicU32 = AtomicU32::new(0);
static MAX: AtomicU32 = AtomicU32::new(100);
/// Valor (%) esperando para ser aplicado; -1 = nada.
static TARGET: AtomicI32 = AtomicI32::new(-1);
static APPLYING: AtomicBool = AtomicBool::new(false);
/// Monitor (HMONITOR) cujo brilho a Central controla: o da barra onde ela foi aberta.
static MONITOR: AtomicIsize = AtomicIsize::new(0);

/// Brilho atual (0..1), se o monitor aceita DDC/CI.
pub fn level() -> Option<f32> {
    let v = LEVEL.load(Ordering::Acquire);
    (v >= 0).then(|| v as f32 / 100.0)
}

/// Chama `f` com o primeiro monitor físico do monitor escolhido em `refresh`.
fn with_monitor<R>(f: impl FnOnce(HANDLE) -> R) -> Option<R> {
    unsafe {
        let mon = HMONITOR(MONITOR.load(Ordering::Acquire) as _);
        let mut n = 0;
        GetNumberOfPhysicalMonitorsFromHMONITOR(mon, &mut n).ok()?;
        let mut list = vec![PHYSICAL_MONITOR::default(); n.max(1) as usize];
        GetPhysicalMonitorsFromHMONITOR(mon, &mut list).ok()?;
        let r = f(list[0].hPhysicalMonitor);
        let _ = DestroyPhysicalMonitors(&list);
        Some(r)
    }
}

/// Relê o brilho em segundo plano e avisa a topbar.
/// O monitor é o que está sob o mouse: a Central acabou de ser aberta pela barra dele.
pub fn refresh() {
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST).0 as isize;
        // Outro monitor: o valor guardado não vale para ele.
        if MONITOR.swap(mon, Ordering::AcqRel) != mon {
            LEVEL.store(-1, Ordering::Release);
        }
    }
    let hwnd = crate::topbar::hwnd().0 as isize;
    std::thread::spawn(move || {
        let read = with_monitor(|h| {
            let (mut min, mut cur, mut max) = (0, 0, 0);
            (unsafe { GetMonitorBrightness(h, &mut min, &mut cur, &mut max) } != 0 && max > min).then_some((min, cur, max))
        })
        .flatten();
        let pct = match read {
            Some((min, cur, max)) => {
                MIN.store(min, Ordering::Release);
                MAX.store(max, Ordering::Release);
                ((cur.saturating_sub(min) * 100) / (max - min)).min(100) as i32
            }
            None => -1,
        };
        LEVEL.store(pct, Ordering::Release);
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as _)), crate::control::WM_UPDATE, WPARAM(0), LPARAM(0));
        }
    });
}

/// Novo brilho (0..1): aparece na hora; a thread aplica o último valor pedido.
pub fn set(v: f32) {
    let pct = (v.clamp(0.0, 1.0) * 100.0).round() as i32;
    LEVEL.store(pct, Ordering::Release);
    TARGET.store(pct, Ordering::Release);
    if APPLYING.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(|| {
        loop {
            let t = TARGET.swap(-1, Ordering::AcqRel);
            if t < 0 {
                APPLYING.store(false, Ordering::Release);
                // Pedido que chegou depois de soltar a vez: continua se ninguém pegou.
                if TARGET.load(Ordering::Acquire) < 0 || APPLYING.swap(true, Ordering::AcqRel) {
                    break;
                }
                continue;
            }
            let (min, max) = (MIN.load(Ordering::Acquire), MAX.load(Ordering::Acquire));
            with_monitor(|h| unsafe { SetMonitorBrightness(h, min + (max - min) * t as u32 / 100) });
        }
    });
}
