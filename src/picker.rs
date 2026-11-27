//! Coletor de cores: uma camada quase invisível cobre as telas enquanto se escolhe; uma
//! janelinha ao lado do cursor mostra a cor e o código. Clique copia (HEX ou RGB, conforme a
//! opção) e guarda na paleta; Esc ou botão direito cancela. Fecha tudo ao terminar.

use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FillRect,
    GetDC, GetPixel, HGDIOBJ, InvalidateRect, PAINTSTRUCT, ReleaseDC, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

static PALETTE: Mutex<Option<Vec<[u8; 3]>>> = Mutex::new(None);
static OVERLAY: AtomicIsize = AtomicIsize::new(0);
static BUBBLE: AtomicIsize = AtomicIsize::new(0);
static CURRENT: Mutex<[u8; 3]> = Mutex::new([0, 0, 0]);

fn file() -> std::path::PathBuf {
    crate::apps::config_path().with_file_name("cores.txt")
}

pub fn palette() -> Vec<[u8; 3]> {
    let Ok(mut p) = PALETTE.lock() else { return Vec::new() };
    p.get_or_insert_with(|| {
        std::fs::read_to_string(file()).unwrap_or_default().lines().filter_map(parse_hex).collect()
    })
    .clone()
}

fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

fn save(list: Vec<[u8; 3]>) {
    let text: String = list.iter().map(|c| format!("{}\n", hex(*c))).collect();
    let _ = std::fs::write(file(), text);
    if let Ok(mut p) = PALETTE.lock() {
        *p = Some(list);
    }
}

pub fn clear() {
    save(Vec::new());
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

fn rgb(c: [u8; 3]) -> String {
    format!("rgb({}, {}, {})", c[0], c[1], c[2])
}

/// No formato escolhido nas Configurações.
pub fn format(c: [u8; 3]) -> String {
    if crate::extras::choice("picker_format") == 1 { rgb(c) } else { hex(c) }
}

pub fn format_other(c: [u8; 3]) -> String {
    if crate::extras::choice("picker_format") == 1 { hex(c) } else { rgb(c) }
}

fn color_at(pt: POINT) -> [u8; 3] {
    unsafe {
        let dc = GetDC(None);
        let c = GetPixel(dc, pt.x, pt.y).0;
        ReleaseDC(None, dc);
        [(c & 0xff) as u8, ((c >> 8) & 0xff) as u8, ((c >> 16) & 0xff) as u8]
    }
}

pub fn start() {
    if OVERLAY.load(Ordering::Relaxed) != 0 {
        return;
    }
    unsafe {
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let cross = LoadCursorW(None, IDC_CROSS).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(overlay_proc),
            hInstance: inst.into(),
            hCursor: cross,
            lpszClassName: w!("ArcDockPicker"),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let wc2 = WNDCLASSW { lpfnWndProc: Some(bubble_proc), hInstance: inst.into(), lpszClassName: w!("ArcDockPickerBubble"), ..Default::default() };
        RegisterClassW(&wc2);
        let (x, y) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        let (cw, ch) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
        let Ok(h) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW,
            w!("ArcDockPicker"),
            w!(""),
            WS_POPUP,
            x,
            y,
            cw,
            ch,
            None,
            None,
            Some(inst.into()),
            None,
        ) else {
            return;
        };
        let _ = SetLayeredWindowAttributes(h, COLORREF(0), 1, LWA_ALPHA);
        let Ok(b) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED,
            w!("ArcDockPickerBubble"),
            w!(""),
            WS_POPUP,
            0,
            0,
            132,
            40,
            None,
            None,
            Some(inst.into()),
            None,
        ) else {
            let _ = DestroyWindow(h);
            return;
        };
        let _ = SetLayeredWindowAttributes(b, COLORREF(0), 245, LWA_ALPHA);
        OVERLAY.store(h.0 as isize, Ordering::Relaxed);
        BUBBLE.store(b.0 as isize, Ordering::Relaxed);
        let _ = ShowWindow(h, SW_SHOW);
        let _ = SetForegroundWindow(h);
        let _ = SetFocus(Some(h));
        follow();
        let _ = ShowWindow(b, SW_SHOWNOACTIVATE);
    }
}

/// Lê a cor sob o cursor e leva a janelinha para perto dele.
fn follow() {
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        if let Ok(mut c) = CURRENT.lock() {
            *c = color_at(pt);
        }
        let b = HWND(BUBBLE.load(Ordering::Relaxed) as _);
        if b.is_invalid() {
            return;
        }
        let _ = SetWindowPos(b, Some(HWND_TOPMOST), pt.x + 18, pt.y + 18, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
        let _ = InvalidateRect(Some(b), None, true);
    }
}

fn finish(pick: bool) {
    unsafe {
        if pick {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let c = color_at(pt);
            crate::clipboard::set_text(&format(c));
            let mut list = palette();
            list.retain(|x| *x != c);
            list.insert(0, c);
            list.truncate(10);
            save(list);
        }
        for a in [&OVERLAY, &BUBBLE] {
            let h = a.swap(0, Ordering::Relaxed);
            if h != 0 {
                let _ = DestroyWindow(HWND(h as _));
            }
        }
    }
}

unsafe extern "system" fn overlay_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_MOUSEMOVE => {
                follow();
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                finish(true);
                LRESULT(0)
            }
            WM_RBUTTONDOWN => {
                finish(false);
                LRESULT(0)
            }
            WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => {
                finish(false);
                LRESULT(0)
            }
            // Perdeu o foco (Alt+Tab etc.): cancela, para a camada não ficar presa na tela.
            WM_ACTIVATE if (wp.0 & 0xffff) == 0 => {
                finish(false);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

unsafe extern "system" fn bubble_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_PAINT {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let c = CURRENT.lock().map(|c| *c).unwrap_or_default();
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let bg = CreateSolidBrush(COLORREF(0x00202020));
            FillRect(dc, &rc, bg);
            let _ = DeleteObject(HGDIOBJ(bg.0));
            let sw = RECT { left: 6, top: 6, right: 34, bottom: rc.bottom - 6 };
            let br = CreateSolidBrush(COLORREF(c[0] as u32 | (c[1] as u32) << 8 | (c[2] as u32) << 16));
            FillRect(dc, &sw, br);
            let _ = DeleteObject(HGDIOBJ(br.0));
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(0x00F0F0F0));
            let mut text: Vec<u16> = hex(c).encode_utf16().collect();
            let mut tr = RECT { left: 38, top: 0, right: rc.right - 4, bottom: rc.bottom };
            DrawTextW(dc, &mut text, &mut tr, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
            let _ = EndPaint(hwnd, &ps);
            return LRESULT(0);
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}
