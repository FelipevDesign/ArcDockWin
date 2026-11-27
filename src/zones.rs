//! Zona de janela (como o modelo que o usuário fez no FancyZones) e sombra extra.
//!
//! Zona: ao arrastar uma janela segurando Shift, aparece de leve a área de trabalho do monitor
//! menos as margens escolhidas; soltando, a janela ocupa essa área. Sem Shift, nada muda.
//! Sombra: atrás de cada janela encaixada, quatro faixas (em volta da janela, não atrás dela
//! toda, para gastar pouca memória) com a sombra desenhada pelo dock. Ela some quando a janela
//! minimiza, maximiza, é arrastada para fora da zona ou fecha.
//!
//! Tudo só existe com a opção ligada: sem ela não há ganchos, timers nem janelas.

use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmGetWindowAttribute,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, BeginPaint, CreateCompatibleDC,
    CreateDIBSection, CreateSolidBrush, DIB_RGB_COLORS, DeleteDC, DeleteObject, EndPaint, FillRect, GetMonitorInfoW,
    HGDIOBJ, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, PAINTSTRUCT, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

use crate::extras;

thread_local! {
    static HOOKS: RefCell<Vec<HWINEVENTHOOK>> = const { RefCell::new(Vec::new()) };
    static SHADOWS: RefCell<Vec<Shadow>> = const { RefCell::new(Vec::new()) };
    /// Última janela solta depois de arrastar, e quando.
    static LAST_END: std::cell::Cell<Option<(isize, std::time::Instant)>> = const { std::cell::Cell::new(None) };
}

/// Janela sendo arrastada, a camada que mostra a zona e o timer que olha o Shift.
static DRAGGING: AtomicIsize = AtomicIsize::new(0);
static OVERLAY: AtomicIsize = AtomicIsize::new(0);
static POLL: AtomicUsize = AtomicUsize::new(0);

pub fn sync() {
    let want = extras::on("zones_on");
    let have = HOOKS.with_borrow(|h| !h.is_empty());
    unsafe {
        if want && !have {
            let mut hooks = Vec::new();
            for (a, b) in [
                (EVENT_SYSTEM_MOVESIZESTART, EVENT_SYSTEM_MOVESIZEEND),
                (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
                (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
                (EVENT_OBJECT_DESTROY, EVENT_OBJECT_DESTROY),
                (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
                (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
            ] {
                let h = SetWinEventHook(a, b, None, Some(event), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
                if !h.is_invalid() {
                    hooks.push(h);
                }
            }
            HOOKS.with_borrow_mut(|v| *v = hooks);
        } else if !want && have {
            HOOKS.with_borrow_mut(|v| {
                for h in v.drain(..) {
                    let _ = UnhookWinEvent(h);
                }
            });
            stop_poll();
            hide_overlay();
        }
    }
    // Sombras: desligada (ou sem zonas) some com todas; mudou a intensidade, redesenha.
    if !want || !extras::on("shadow_on") {
        shadows(|v| v.clear());
    } else {
        shadows(|v| v.retain_mut(|s| s.update(true)));
        scan();
    }
}

/// Área da zona no monitor do ponto `pt` (área de trabalho menos as margens, na escala dele).
fn zone_at(pt: POINT) -> Option<RECT> {
    unsafe {
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(mon, &mut mi).as_bool() {
            return None;
        }
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let k = dx as f32 / 96.0;
        let m = |key: &str| (extras::num(key) * k).round() as i32;
        let w = mi.rcWork;
        let r = RECT { left: w.left + m("zone_left"), top: w.top + m("zone_top"), right: w.right - m("zone_right"), bottom: w.bottom - m("zone_bottom") };
        (r.right - r.left > 100 && r.bottom - r.top > 100).then_some(r)
    }
}

/// Mexe nas sombras sem nunca travar (um aviso pode chegar no meio de outro).
fn shadows(f: impl FnOnce(&mut Vec<Shadow>)) {
    SHADOWS.with(|s| {
        if let Ok(mut v) = s.try_borrow_mut() {
            f(&mut v);
        }
    });
}

fn shift() -> bool {
    unsafe { GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000 != 0 }
}

fn cursor() -> POINT {
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    pt
}

unsafe extern "system" fn event(_: HWINEVENTHOOK, ev: u32, hwnd: HWND, obj: i32, child: i32, _: u32, _: u32) {
    if hwnd.is_invalid() || obj != OBJID_WINDOW.0 || child != CHILDID_SELF as i32 {
        return;
    }
    match ev {
        EVENT_SYSTEM_MOVESIZESTART => {
            DRAGGING.store(hwnd.0 as isize, Ordering::Relaxed);
            start_poll();
            shadows(|v| v.iter_mut().filter(|s| s.target == hwnd).for_each(|s| s.hide()));
        }
        EVENT_SYSTEM_MOVESIZEEND => {
            stop_poll();
            let shown = OVERLAY.load(Ordering::Relaxed) != 0 && unsafe { IsWindowVisible(HWND(OVERLAY.load(Ordering::Relaxed) as _)).as_bool() };
            hide_overlay();
            DRAGGING.store(0, Ordering::Relaxed);
            if shown && shift() && let Some(z) = zone_at(cursor()) {
                snap(hwnd, z);
            }
            // Encaixada por nós ou por outro programa (FancyZones): ganha sombra; fora, perde.
            // O FancyZones encaixa logo depois de soltar: olha de novo nos próximos instantes.
            LAST_END.set(Some((hwnd.0 as isize, std::time::Instant::now())));
            track(hwnd);
            shadows(|v| v.retain_mut(|s| if s.target == hwnd { s.update(true) } else { true }));
        }
        EVENT_OBJECT_DESTROY => shadows(|v| v.retain(|s| s.target != hwnd)),
        _ => {
            let recent = LAST_END.get().is_some_and(|(h, t)| h == hwnd.0 as isize && t.elapsed().as_millis() < 1500);
            if ev == EVENT_SYSTEM_FOREGROUND || ev == EVENT_SYSTEM_MINIMIZEEND || (ev == EVENT_OBJECT_LOCATIONCHANGE && recent) {
                track(hwnd);
            }
            shadows(|v| {
                // Primeiro plano mudou: cada sombra volta para trás da sua janela. Saiu da zona
                // (Win+setas, maximizada...): a sombra some.
                let all = ev == EVENT_SYSTEM_FOREGROUND;
                v.retain_mut(|s| if all || s.target == hwnd { s.update(false) } else { true });
            });
        }
    }
}

/// A janela está na zona do monitor dela (com folga de alguns pixels)?
fn in_zone(hwnd: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() || IsZoomed(hwnd).as_bool() {
            return false;
        }
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        if style & WS_THICKFRAME.0 == 0 || style & WS_CHILD.0 != 0 {
            return false;
        }
    }
    let Some(f) = frame(hwnd) else { return false };
    let c = POINT { x: (f.left + f.right) / 2, y: (f.top + f.bottom) / 2 };
    let Some(z) = zone_at(c) else { return false };
    let tol = (12.0 * crate::panel_scale()) as i32;
    crate::config::debug_log(&format!(
        "zona: janela {:?} quadro ({},{},{},{}) zona ({},{},{},{})",
        hwnd.0, f.left, f.top, f.right, f.bottom, z.left, z.top, z.right, z.bottom
    ));
    (f.left - z.left).abs() <= tol && (f.top - z.top).abs() <= tol && (f.right - z.right).abs() <= tol && (f.bottom - z.bottom).abs() <= tol
}

/// Dá sombra à janela se ela está na zona (e tira se não está mais).
fn track(hwnd: HWND) {
    if !extras::on("shadow_on") {
        return;
    }
    let inside = in_zone(hwnd);
    shadows(|v| {
        let has = v.iter().any(|s| s.target == hwnd);
        if inside && !has {
            let s = Shadow::new(hwnd);
            crate::config::debug_log(&format!("sombra: criar {:?} -> {}", hwnd.0, s.is_some()));
            if let Some(s) = s {
                v.push(s);
            }
        } else if !inside && has {
            v.retain(|s| s.target != hwnd);
        }
    });
}

/// Ao ligar: janelas que já estão na zona.
fn scan() {
    unsafe extern "system" fn each(h: HWND, _: LPARAM) -> windows::core::BOOL {
        track(h);
        windows::core::BOOL(1)
    }
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(0));
    }
}

/// Encaixa `hwnd` na zona `z` (o retângulo visível; as bordas invisíveis do Windows 11 ficam fora).
fn snap(hwnd: HWND, z: RECT) {
    unsafe {
        if IsZoomed(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let mut wr = RECT::default();
        let _ = GetWindowRect(hwnd, &mut wr);
        let fr = frame(hwnd).unwrap_or(wr);
        let (l, t, r, b) = (fr.left - wr.left, fr.top - wr.top, wr.right - fr.right, wr.bottom - fr.bottom);
        let _ = SetWindowPos(
            hwnd,
            None,
            z.left - l,
            z.top - t,
            z.right - z.left + l + r,
            z.bottom - z.top + t + b,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// Retângulo visível da janela (sem as bordas invisíveis).
fn frame(hwnd: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    unsafe {
        DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut _ as *mut _, size_of::<RECT>() as u32).ok()?;
    }
    Some(r)
}

fn start_poll() {
    if POLL.load(Ordering::Relaxed) == 0 {
        let id = unsafe { SetTimer(None, 0, 30, Some(poll)) };
        POLL.store(id, Ordering::Relaxed);
    }
}

fn stop_poll() {
    let id = POLL.swap(0, Ordering::Relaxed);
    if id != 0 {
        unsafe {
            let _ = KillTimer(None, id);
        }
    }
}

/// Durante o arrasto: com Shift, mostra a zona do monitor sob o cursor; sem, esconde.
unsafe extern "system" fn poll(_: HWND, _: u32, _: usize, _: u32) {
    if DRAGGING.load(Ordering::Relaxed) == 0 {
        stop_poll();
        return;
    }
    match zone_at(cursor()).filter(|_| shift()) {
        Some(z) => show_overlay(z),
        None => hide_overlay(),
    }
}

fn accent() -> u32 {
    // COLORREF é 0x00BBGGRR; azul discreto do Windows.
    0x00D77800
}

fn show_overlay(z: RECT) {
    unsafe {
        let mut h = HWND(OVERLAY.load(Ordering::Relaxed) as _);
        if h.is_invalid() {
            let inst = GetModuleHandleW(None).unwrap_or_default();
            let wc = WNDCLASSW { lpfnWndProc: Some(overlay_proc), hInstance: inst.into(), lpszClassName: w!("ArcDockZone"), ..Default::default() };
            RegisterClassW(&wc);
            let Ok(nh) = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                w!("ArcDockZone"),
                w!(""),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(inst.into()),
                None,
            ) else {
                return;
            };
            let round = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(nh, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as *const _, 4);
            OVERLAY.store(nh.0 as isize, Ordering::Relaxed);
            h = nh;
        }
        let a = (extras::num("zone_flash") / 100.0 * 255.0).round() as u8;
        let _ = SetLayeredWindowAttributes(h, COLORREF(0), a, LWA_ALPHA);
        let _ = SetWindowPos(
            h,
            Some(HWND_TOPMOST),
            z.left,
            z.top,
            z.right - z.left,
            z.bottom - z.top,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

fn hide_overlay() {
    let h = OVERLAY.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = ShowWindow(HWND(h as _), SW_HIDE);
        }
    }
}

unsafe extern "system" fn overlay_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_PAINT {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let br = CreateSolidBrush(COLORREF(accent()));
            FillRect(dc, &rc, br);
            let _ = DeleteObject(HGDIOBJ(br.0));
            let _ = EndPaint(hwnd, &ps);
            return LRESULT(0);
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

// ---------------------------------------------------------------------------------------------
// Sombra

struct Shadow {
    target: HWND,
    /// Faixas de cima, de baixo, da esquerda e da direita.
    strips: [HWND; 4],
    /// Retângulo e parâmetros do último desenho.
    last: Option<(RECT, i32, f32)>,
}

impl Shadow {
    fn new(target: HWND) -> Option<Self> {
        unsafe {
            let inst = GetModuleHandleW(None).unwrap_or_default();
            let wc = WNDCLASSW { lpfnWndProc: Some(plain_proc), hInstance: inst.into(), lpszClassName: w!("ArcDockShadow"), ..Default::default() };
            RegisterClassW(&wc);
            let mut strips = [HWND::default(); 4];
            for s in &mut strips {
                *s = CreateWindowExW(
                    WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    w!("ArcDockShadow"),
                    w!(""),
                    WS_POPUP,
                    0,
                    0,
                    1,
                    1,
                    None,
                    None,
                    Some(inst.into()),
                    None,
                )
                .ok()?;
            }
            let mut s = Self { target, strips, last: None };
            s.update(true);
            Some(s)
        }
    }

    fn hide(&self) {
        for &h in &self.strips {
            unsafe {
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
    }

    /// Acompanha a janela: posição, tamanho, ordem e visibilidade. `force` redesenha.
    /// Devolve false quando a janela saiu da zona (a sombra deve sumir).
    fn update(&mut self, force: bool) -> bool {
        unsafe {
            let t = self.target;
            if !IsWindow(Some(t)).as_bool() {
                return false;
            }
            let mut cloaked = 0u32;
            let _ = DwmGetWindowAttribute(t, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut _, 4);
            if !IsWindowVisible(t).as_bool() || IsIconic(t).as_bool() || cloaked != 0 {
                self.hide();
                return true;
            }
            // Sendo arrastada: escondida até soltar (aí decide se continua na zona).
            if DRAGGING.load(Ordering::Relaxed) == t.0 as isize {
                self.hide();
                return true;
            }
            if !in_zone(t) {
                return false;
            }
            let Some(r) = frame(t) else { return true };
            let soft = (extras::num("shadow_soft") * crate::panel_scale()).round() as i32;
            let level = extras::num("shadow_level") / 100.0;
            let same = self.last.is_some_and(|(lr, ls, ll)| {
                lr.right - lr.left == r.right - r.left && lr.bottom - lr.top == r.bottom - r.top && ls == soft && ll == level
            });
            let m = soft.max(1);
            let rects = [
                RECT { left: r.left - m, top: r.top - m, right: r.right + m, bottom: r.top },
                RECT { left: r.left - m, top: r.bottom, right: r.right + m, bottom: r.bottom + m },
                RECT { left: r.left - m, top: r.top, right: r.left, bottom: r.bottom },
                RECT { left: r.right, top: r.top, right: r.right + m, bottom: r.bottom },
            ];
            for (k, &h) in self.strips.iter().enumerate() {
                let sr = rects[k];
                if force || !same {
                    paint_strip(h, sr, r, m, level);
                } else {
                    let _ = SetWindowPos(h, None, sr.left, sr.top, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER);
                }
                // Logo atrás da janela.
                let _ = SetWindowPos(h, Some(t), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
            }
            self.last = Some((r, soft, level));
        }
        true
    }
}

impl Drop for Shadow {
    fn drop(&mut self) {
        for &h in &self.strips {
            unsafe {
                let _ = DestroyWindow(h);
            }
        }
    }
}

/// Desenha a faixa `sr` da sombra da janela `win` (alcance `m`, força `level`) e a põe na tela.
fn paint_strip(h: HWND, sr: RECT, win: RECT, m: i32, level: f32) {
    let (w, hh) = (sr.right - sr.left, sr.bottom - sr.top);
    if w <= 0 || hh <= 0 {
        return;
    }
    unsafe {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -hh,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let Ok(dib) = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else { return };
        let px = std::slice::from_raw_parts_mut(bits as *mut u8, (w * hh * 4) as usize);
        // A sombra cai um pouco para baixo.
        let off = m / 4;
        let (l, t, r, b) = (win.left, win.top + off, win.right, win.bottom + off);
        for y in 0..hh {
            let sy = sr.top + y;
            let dy = (t - sy).max(sy - b).max(0) as f32;
            for x in 0..w {
                let sx = sr.left + x;
                let dx = (l - sx).max(sx - r).max(0) as f32;
                let d = (dx * dx + dy * dy).sqrt();
                let inside = dx == 0.0 && dy == 0.0;
                let f = if inside { 1.0 } else { (1.0 - d / m as f32).max(0.0) };
                // Mais escura colada na janela, sumindo devagar (curva parecida com a do macOS).
                let a = (level * 235.0 * f.powf(1.7)).round().min(255.0) as u8;
                let i = ((y * w + x) * 4) as usize;
                px[i..i + 4].copy_from_slice(&[0, 0, 0, a]);
            }
        }
        let mem = CreateCompatibleDC(None);
        let old = SelectObject(mem, HGDIOBJ(dib.0));
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        let pos = POINT { x: sr.left, y: sr.top };
        let size = SIZE { cx: w, cy: hh };
        let _ = UpdateLayeredWindow(h, None, Some(&pos), Some(&size), Some(mem), Some(&POINT::default()), COLORREF(0), Some(&blend), ULW_ALPHA);
        SelectObject(mem, old);
        let _ = DeleteDC(mem);
        let _ = DeleteObject(HGDIOBJ(dib.0));
    }
}

unsafe extern "system" fn plain_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}
