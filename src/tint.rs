//! Cor do app maximizado: quando a janela mais à frente logo abaixo da topbar está
//! maximizada, lê a primeira linha de pixels dela na tela e devolve a cor que mais aparece.
//! A topbar usa essa cor como fundo, para emendar com a barra de título do app.

use std::collections::HashMap;
use std::ffi::c_void;

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GetDC, HGDIOBJ, ReleaseDC, SRCCOPY, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetClassNameW, GetWindowLongW, IsIconic, IsWindowVisible, IsZoomed,
    WS_EX_TOOLWINDOW,
};
use windows::core::BOOL;

struct Search {
    /// Faixa logo abaixo da barra (coordenadas de tela).
    strip: RECT,
    found: Option<HWND>,
}

unsafe extern "system" fn find_proc(h: HWND, lp: LPARAM) -> BOOL {
    let s = unsafe { &mut *(lp.0 as *mut Search) };
    unsafe {
        if !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() {
            return BOOL(1);
        }
        if GetWindowLongW(h, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
            return BOOL(1);
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut c_void, 4);
        if cloaked != 0 {
            return BOOL(1);
        }
        let mut class = [0u16; 32];
        let n = GetClassNameW(h, &mut class) as usize;
        let class = String::from_utf16_lossy(&class[..n]);
        if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
            return BOOL(1);
        }
        let mut r = RECT::default();
        if DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut _ as *mut c_void, 16).is_err() {
            return BOOL(1);
        }
        let st = &s.strip;
        if r.left < st.right && r.right > st.left && r.top < st.bottom && r.bottom > st.top {
            // A primeira janela (em z-order) sobre a faixa decide: maximizada ou não.
            if IsZoomed(h).as_bool() {
                s.found = Some(h);
            }
            return BOOL(0);
        }
    }
    BOOL(1)
}

/// Cor (RGB) da faixa logo abaixo de `bar`, se a janela da frente ali estiver maximizada.
/// As janelas do próprio processo (topbar, dock, painéis) são TOOLWINDOW e ficam de fora.
pub fn sample(bar: RECT) -> Option<[u8; 3]> {
    let w = bar.right - bar.left;
    let mut s = Search {
        strip: RECT { left: bar.left, top: bar.bottom, right: bar.right, bottom: bar.bottom + 4 },
        found: None,
    };
    unsafe {
        let _ = EnumWindows(Some(find_proc), LPARAM(&mut s as *mut _ as isize));
    }
    s.found?;

    // Uma linha de pixels 2 px abaixo da barra (pula uma eventual borda de 1 px).
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -1,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let result = match CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(dib) => {
                let old = SelectObject(mem, HGDIOBJ(dib.0));
                let ok = BitBlt(mem, 0, 0, w, 1, Some(screen), bar.left, bar.bottom + 2, SRCCOPY).is_ok();
                let out = if ok {
                    let px = std::slice::from_raw_parts(bits as *const u8, w as usize * 4);
                    dominant(px)
                } else {
                    None
                };
                SelectObject(mem, old);
                let _ = DeleteObject(HGDIOBJ(dib.0));
                out
            }
            Err(_) => None,
        };
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        result
    }
}

/// Cor mais frequente da linha (BGRA), contando cada 4º pixel.
fn dominant(px: &[u8]) -> Option<[u8; 3]> {
    let mut count: HashMap<[u8; 3], u32> = HashMap::new();
    let mut total = 0;
    for p in px.chunks_exact(4).step_by(4) {
        *count.entry([p[2], p[1], p[0]]).or_default() += 1;
        total += 1;
    }
    let (c, n) = count.into_iter().max_by_key(|(_, n)| *n)?;
    // Sem cor dominante (imagem, gradiente): não pinta.
    (n * 4 >= total).then_some(c)
}
