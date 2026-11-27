//! Histórico da área de transferência (só texto, só na memória) e utilidades de copiar:
//! texto e listas de arquivos. Com a ferramenta desligada não há janela nem aviso registrado.
//! Conteúdo marcado pelos gerenciadores de senha para ficar fora do histórico é ignorado.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
    OpenClipboard, RegisterClipboardFormatW, RemoveClipboardFormatListener, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, HWND_MESSAGE, RegisterClassW, WINDOW_EX_STYLE, WINDOW_STYLE,
    WNDCLASSW,
};
use windows::core::w;

const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const WM_CLIPBOARDUPDATE: u32 = 0x031D;

struct Entry {
    text: String,
    pinned: bool,
}

static HISTORY: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
static PAUSED: AtomicBool = AtomicBool::new(false);
/// O próximo aviso é de uma cópia nossa (não entra de novo no topo).
static OWN: AtomicBool = AtomicBool::new(false);
static WINDOW: AtomicIsize = AtomicIsize::new(0);

pub fn paused() -> bool {
    PAUSED.load(Ordering::Relaxed)
}

pub fn set_paused(on: bool) {
    PAUSED.store(on, Ordering::Relaxed);
}

/// Liga ou desliga a escuta conforme a opção.
pub fn sync() {
    let want = crate::extras::on("tool_clip");
    let cur = WINDOW.load(Ordering::Relaxed);
    unsafe {
        if want && cur == 0 {
            let inst = GetModuleHandleW(None).unwrap_or_default();
            let wc = WNDCLASSW { lpfnWndProc: Some(proc_), hInstance: inst.into(), lpszClassName: w!("ArcDockClipboard"), ..Default::default() };
            RegisterClassW(&wc);
            let Ok(h) = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("ArcDockClipboard"),
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
            let _ = AddClipboardFormatListener(h);
            WINDOW.store(h.0 as isize, Ordering::Relaxed);
        } else if !want && cur != 0 {
            let h = HWND(cur as _);
            let _ = RemoveClipboardFormatListener(h);
            let _ = DestroyWindow(h);
            WINDOW.store(0, Ordering::Relaxed);
            if let Ok(mut h) = HISTORY.lock() {
                h.clear();
            }
        }
    }
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_CLIPBOARDUPDATE {
        if OWN.swap(false, Ordering::Relaxed) || paused() {
            return LRESULT(0);
        }
        if let Some(t) = read_text() {
            push(t);
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn push(text: String) {
    if text.trim().is_empty() {
        return;
    }
    let limit = crate::extras::num("clip_limit") as usize;
    let Ok(mut h) = HISTORY.lock() else { return };
    let pinned = h.iter().position(|e| e.text == text).map(|i| h.remove(i).pinned).unwrap_or(false);
    h.insert(0, Entry { text, pinned });
    // Corta os mais antigos não fixados.
    while h.len() > limit {
        match h.iter().rposition(|e| !e.pinned) {
            Some(i) => {
                h.remove(i);
            }
            None => break,
        }
    }
}

/// Texto da área de transferência, se não for de quem pediu para ficar fora do histórico.
fn read_text() -> Option<String> {
    unsafe {
        let skip = [w!("ExcludeClipboardContentFromMonitorProcessing"), w!("CanIncludeInClipboardHistory")];
        let ex = RegisterClipboardFormatW(skip[0]);
        if IsClipboardFormatAvailable(ex).is_ok() {
            return None;
        }
        // CanIncludeInClipboardHistory = 0 também pede para não guardar (só checa a presença).
        let can = RegisterClipboardFormatW(skip[1]);
        let block_by_can = IsClipboardFormatAvailable(can).is_ok();
        if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
            return None;
        }
        OpenClipboard(None).ok()?;
        let mut out = None;
        if block_by_can && let Ok(h) = GetClipboardData(can) {
            let p = GlobalLock(HGLOBAL(h.0)) as *const u32;
            let zero = !p.is_null() && *p == 0;
            let _ = GlobalUnlock(HGLOBAL(h.0));
            if zero {
                let _ = CloseClipboard();
                return None;
            }
        }
        if let Ok(h) = GetClipboardData(CF_UNICODETEXT) {
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if !p.is_null() {
                let max = GlobalSize(g) / 2;
                let n = (0..max).take_while(|&i| *p.add(i) != 0).count();
                // Até 64 mil caracteres (o resto não entra no histórico).
                let n = n.min(65536);
                out = Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, n)));
                let _ = GlobalUnlock(g);
            }
        }
        let _ = CloseClipboard();
        out
    }
}

/// Histórico: texto e se está fixado (fixados primeiro).
pub fn items() -> Vec<(String, bool)> {
    let Ok(h) = HISTORY.lock() else { return Vec::new() };
    let mut v: Vec<(String, bool)> = h.iter().filter(|e| e.pinned).map(|e| (e.text.clone(), true)).collect();
    v.extend(h.iter().filter(|e| !e.pinned).map(|e| (e.text.clone(), false)));
    v
}

pub fn reuse(i: usize) {
    if let Some((t, _)) = items().get(i) {
        let t = t.clone();
        set_text(&t);
        // Volta para o topo dos não fixados.
        if let Ok(mut h) = HISTORY.lock()
            && let Some(k) = h.iter().position(|e| e.text == t)
        {
            let e = h.remove(k);
            h.insert(0, e);
        }
    }
}

pub fn toggle_pin(i: usize) {
    let Some((t, _)) = items().get(i).cloned() else { return };
    if let Ok(mut h) = HISTORY.lock()
        && let Some(e) = h.iter_mut().find(|e| e.text == t)
    {
        e.pinned = !e.pinned;
    }
}

pub fn clear() {
    if let Ok(mut h) = HISTORY.lock() {
        h.retain(|e| e.pinned);
    }
}

unsafe fn put(format: u32, bytes: &[u8]) -> bool {
    unsafe {
        let Ok(g) = GlobalAlloc(GMEM_MOVEABLE, bytes.len()) else { return false };
        let p = GlobalLock(g) as *mut u8;
        if p.is_null() {
            return false;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        let _ = GlobalUnlock(g);
        if OpenClipboard(None).is_err() {
            return false;
        }
        let _ = EmptyClipboard();
        OWN.store(true, Ordering::Relaxed);
        let ok = SetClipboardData(format, Some(HANDLE(g.0))).is_ok();
        let _ = CloseClipboard();
        ok
    }
}

/// Copia um texto.
pub fn set_text(t: &str) {
    let mut b: Vec<u8> = t.encode_utf16().chain([0]).flat_map(|c| c.to_le_bytes()).collect();
    b.extend([0, 0]);
    unsafe {
        put(CF_UNICODETEXT, &b);
    }
}

/// Copia arquivos (para colar no Explorador).
pub fn set_files(paths: &[PathBuf]) {
    if paths.is_empty() {
        return;
    }
    // DROPFILES { pFiles: 20, pt, fNC, fWide: 1 } + lista de caminhos terminada em dois zeros.
    let mut b: Vec<u8> = Vec::new();
    b.extend(20u32.to_le_bytes());
    b.extend([0u8; 12]);
    b.extend(1u32.to_le_bytes());
    for p in paths {
        for c in p.to_string_lossy().encode_utf16().chain([0]) {
            b.extend(c.to_le_bytes());
        }
    }
    b.extend([0, 0]);
    unsafe {
        put(CF_HDROP, &b);
    }
}

pub fn has_files() -> bool {
    unsafe { IsClipboardFormatAvailable(CF_HDROP).is_ok() }
}

/// Arquivos copiados no Explorador.
pub fn files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    unsafe {
        if OpenClipboard(None).is_err() {
            return out;
        }
        if let Ok(h) = GetClipboardData(CF_HDROP) {
            let drop = HDROP(h.0);
            let n = DragQueryFileW(drop, u32::MAX, None);
            for i in 0..n {
                let mut buf = [0u16; 1024];
                let len = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
                out.push(PathBuf::from(String::from_utf16_lossy(&buf[..len])));
            }
        }
        let _ = CloseClipboard();
    }
    out
}
