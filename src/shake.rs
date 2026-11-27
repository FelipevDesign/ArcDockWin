//! Sacudir o mouse para achar o cursor (como no macOS): mexer rápido de um lado para o outro
//! aumenta o cursor por um instante; parando, ele volta ao normal. Não precisa de
//! administrador. Um gancho leve de mouse numa thread própria só existe com a opção ligada.
//!
//! O cursor grande vem do próprio esquema de cursores do usuário (Painel de Controle), só que
//! carregado num tamanho maior; para voltar, o Windows recarrega o esquema
//! (`SPI_SETCURSORS`). Se o dock fechar com o cursor grande, a próxima abertura desfaz.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HANDLE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR};

static THREAD: AtomicU32 = AtomicU32::new(0);
static BIG: AtomicBool = AtomicBool::new(false);
/// Mensagens para a thread do gancho (o trabalho pesado sai de dentro do gancho).
const GROW: u32 = WM_APP + 10;

struct Track {
    /// (instante, x) dos últimos movimentos.
    pts: VecDeque<(Instant, i32)>,
    last_shake: Option<Instant>,
}

static TRACK: Mutex<Track> = Mutex::new(Track { pts: VecDeque::new(), last_shake: None });

fn marker() -> std::path::PathBuf {
    crate::apps::config_path().with_file_name("cursor-grande.tmp")
}

pub fn sync() {
    // Cursor ficou grande numa sessão anterior (o dock fechou no meio): volta ao normal.
    if marker().exists() && !BIG.load(Ordering::Relaxed) {
        restore();
    }
    if !crate::extras::on("cur_shake") {
        let tid = THREAD.swap(0, Ordering::AcqRel);
        if tid != 0 && tid != u32::MAX {
            unsafe {
                let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
    } else if THREAD.compare_exchange(0, u32::MAX, Ordering::AcqRel, Ordering::Acquire).is_ok() {
        std::thread::spawn(hook_thread);
    }
}

/// Ao sair do dock: nunca deixar o cursor grande.
pub fn shutdown() {
    if BIG.load(Ordering::Relaxed) {
        restore();
    }
}

fn hook_thread() {
    unsafe {
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        if THREAD.compare_exchange(u32::MAX, GetCurrentThreadId(), Ordering::AcqRel, Ordering::Acquire).is_err() {
            return;
        }
        let module = GetModuleHandleW(None).ok().map(Into::into);
        let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module, 0);
        // Volta ao normal ~0,6 s depois da última sacudida.
        let timer = SetTimer(None, 0, 100, None);
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            match msg.message {
                // Fora da thread do gancho: ele não pode ficar parado (o mouse travaria).
                GROW if !BIG.swap(true, Ordering::Relaxed) => {
                    std::thread::spawn(grow);
                }
                WM_TIMER if BIG.load(Ordering::Relaxed) => {
                    let idle = TRACK.lock().ok().and_then(|t| t.last_shake).is_none_or(|t| t.elapsed() > Duration::from_millis(650));
                    if idle {
                        BIG.store(false, Ordering::Relaxed);
                        std::thread::spawn(restore);
                    }
                }
                _ => {}
            }
        }
        let _ = KillTimer(None, timer);
        if let Ok(h) = hook {
            let _ = UnhookWindowsHookEx(h);
        }
        if BIG.load(Ordering::Relaxed) {
            restore();
        }
    }
}

unsafe extern "system" fn mouse_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32 && wp.0 as u32 == WM_MOUSEMOVE {
            let m = &*(lp.0 as *const MSLLHOOKSTRUCT);
            if shaken(m.pt.x) {
                let tid = THREAD.load(Ordering::Relaxed);
                let _ = PostThreadMessageW(tid, GROW, WPARAM(0), LPARAM(0));
            }
        }
        CallNextHookEx(None, code, wp, lp)
    }
}

/// Sacudida: várias trocas de direção, cada trecho com uma distância mínima, em pouco tempo.
fn shaken(x: i32) -> bool {
    let now = Instant::now();
    let Ok(mut t) = TRACK.lock() else { return false };
    t.pts.push_back((now, x));
    while t.pts.front().is_some_and(|p| now - p.0 > Duration::from_millis(450)) {
        t.pts.pop_front();
    }
    // Sensibilidade 1..10: menos trocas e trechos menores com sensibilidade alta.
    let sens = crate::extras::num("cur_shake_sens").clamp(1.0, 10.0);
    let need = (7.0 - sens * 0.4).round() as usize; // 6 .. 3 trocas
    let min_run = 90.0 - sens * 6.0; // 84 .. 30 px por trecho
    let mut turns = 0;
    let mut dir = 0i32;
    let mut run = 0.0f32;
    let mut prev = None::<i32>;
    for &(_, px) in &t.pts {
        if let Some(p) = prev {
            let d = px - p;
            if d != 0 {
                let s = d.signum();
                if dir != 0 && s != dir {
                    if run >= min_run {
                        turns += 1;
                    }
                    run = 0.0;
                }
                dir = s;
                run += d.abs() as f32;
            }
        }
        prev = Some(px);
    }
    let shaking = turns >= need;
    if shaking || (BIG.load(Ordering::Relaxed) && turns >= 1) {
        t.last_shake = Some(now);
    }
    shaking
}

/// Cursores aumentados: nome no registro (esquema do usuário) e id do sistema.
const CURSORS: [(&str, u32); 4] = [("Arrow", 32512), ("IBeam", 32513), ("Hand", 32649), ("AppStarting", 32650)];

fn scheme_file(name: &str) -> Option<String> {
    let mut buf = [0u16; 520];
    let mut len = (buf.len() * 2) as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from("Control Panel\\Cursors"),
            &HSTRING::from(name),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut len),
        )
        .ok()
        .ok()?;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
    let s = String::from_utf16_lossy(&buf[..n]);
    // %SystemRoot%\cursors\... → caminho de verdade.
    let s = s.replace("%SystemRoot%", &std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()));
    let s = s.replace("%SYSTEMROOT%", &std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()));
    (!s.trim().is_empty()).then_some(s)
}

/// Aumenta em poucos passos (crescendo, discreto) até o tamanho escolhido.
fn grow() {
    let _ = std::fs::write(marker(), "1");
    let base = unsafe { GetSystemMetrics(SM_CXCURSOR) }.max(32);
    let k = crate::extras::num("cur_shake_size") / 100.0;
    let target = ((base as f32) * k).round() as i32;
    for step in 1..=4 {
        let size = base + (target - base) * step / 4;
        for (name, id) in CURSORS {
            let h = unsafe {
                match scheme_file(name) {
                    Some(f) => LoadImageW(None, &HSTRING::from(f), IMAGE_CURSOR, size, size, LR_LOADFROMFILE),
                    None => LoadImageW(None, PCWSTR(id as usize as _), IMAGE_CURSOR, size, size, LR_SHARED),
                }
            };
            if let Ok(h) = h {
                unsafe {
                    // LR_SHARED não pode ser entregue: copia antes.
                    let own = CopyImage(HANDLE(h.0), IMAGE_CURSOR, size, size, IMAGE_FLAGS(0)).unwrap_or(HANDLE(h.0));
                    let _ = SetSystemCursor(HCURSOR(own.0), SYSTEM_CURSOR_ID(id));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Volta aos cursores do esquema do usuário.
fn restore() {
    unsafe {
        let _ = SystemParametersInfoW(SPI_SETCURSORS, 0, None, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
    BIG.store(false, Ordering::Relaxed);
    let _ = std::fs::remove_file(marker());
}

