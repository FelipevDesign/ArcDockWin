//! Topbar: barra fina no topo da tela principal, estilo barra de menus do macOS.
//! Mesmo padrão do dock: janela de vidro atrás + janela em camadas por cima. A tinta, a
//! linha de baixo, o realce e os ícones da bandeja saem do tiny-skia; texto e glifos de
//! status são desenhados com DirectWrite (Direct2D ligado ao memdc da DIB).

use std::cell::{Cell, RefCell};
use std::ffi::c_void;

use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, PixmapPaint, Rect, Transform};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory,
    ID2D1DCRenderTarget, ID2D1Factory,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory,
    IDWriteFontCollection, IDWriteTextFormat, IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, HDC, HGDIOBJ, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
    TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VIRTUAL_KEY, VK_LWIN,
};
use windows::Win32::UI::Shell::{
    ABE_TOP, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, ABN_FULLSCREENAPP, ABN_POSCHANGED,
    APPBARDATA, SHAppBarMessage,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, w};
use windows_numerics::Vector2;

use crate::glass;
use crate::status::{self, Audio, Net, Wlan};

/// Botão usado num clique de ícone da bandeja.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayButton {
    Left,
    Right,
    Double,
}

/// Callback de clique na bandeja: índice do ícone, botão e ponto (tela) onde menus devem abrir.
pub type TrayClick = fn(index: usize, button: TrayButton, anchor: POINT);

const WM_APPBAR: u32 = WM_APP + 1;
const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_CLOCK: usize = 1;
const TIMER_POLL: usize = 2;
/// Intervalo da consulta de rede e bateria (ms).
const POLL_MS: u32 = 3000;

/// Altura da barra, espaço interno de cada item e largura de um glifo (px antes da escala).
const HEIGHT: f32 = 24.0;
const PAD: f32 = 6.0;
const GLYPH_W: f32 = 16.0;
const TRAY_PAD: f32 = 4.0;

thread_local! {
    static BAR: RefCell<Option<Bar>> = const { RefCell::new(None) };
    static TRAY_CLICK: Cell<Option<TrayClick>> = const { Cell::new(None) };
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Tray(usize),
    Network,
    Volume,
    Battery,
    Clock,
}

/// Item desenhado: faixa horizontal [l, r) e textos (layout, x, opacidade).
struct Item {
    kind: Kind,
    l: f32,
    r: f32,
    text: Vec<(IDWriteTextLayout, f32, f32)>,
}

struct Bar {
    hwnd: HWND,
    /// Janela atrás que só fornece o desfoque (vidro).
    glass: HWND,
    _glass_fx: Option<glass::Glass>,
    scale: f32,
    /// Posição atual na tela (dada pelo sistema como AppBar).
    rect: RECT,
    pixmap: Pixmap,
    memdc: HDC,
    bits: *mut u8,
    target: ID2D1DCRenderTarget,
    dwrite: IDWriteFactory,
    text_fmt: IDWriteTextFormat,
    icon_fmt: IDWriteTextFormat,
    /// Ícones da bandeja, já no tamanho certo (~16 px × escala).
    tray: Vec<Pixmap>,
    items: Vec<Item>,
    hot: Option<Kind>,
    tracking: bool,
    /// App em tela cheia: a barra sai da frente.
    fullscreen: bool,
    clock: String,
    battery: Option<u16>,
    volume: Option<(f32, bool)>,
    net: Net,
    audio: Option<Audio>,
    wlan: Wlan,
}

impl Bar {
    unsafe fn new(hwnd: HWND, glass_hwnd: HWND) -> windows::core::Result<Self> {
        unsafe {
            let scale = GetDpiForWindow(hwnd) as f32 / 96.0;
            let (w, _) = crate::taskbar::screen_size();
            let h = (HEIGHT * scale).round() as i32;

            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let dib = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let memdc = CreateCompatibleDC(None);
            SelectObject(memdc, HGDIOBJ(dib.0));

            // Direct2D em software desenha direto na DIB, por cima do que já está nela, sem
            // dispositivo de GPU (96 dpi: 1 DIP = 1 px; a escala vai nos tamanhos).
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                ..Default::default()
            };
            let target = factory.CreateDCRenderTarget(&props)?;
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);

            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let mut fonts: Option<IDWriteFontCollection> = None;
            dwrite.GetSystemFontCollection(&mut fonts, false)?;
            let has = |name: PCWSTR| {
                let (mut i, mut exists) = (0u32, BOOL(0));
                fonts.as_ref().is_some_and(|f| f.FindFamilyName(name, &mut i, &mut exists).is_ok() && exists.as_bool())
            };
            let family = if has(w!("Segoe UI Variable Text")) { w!("Segoe UI Variable Text") } else { w!("Segoe UI") };
            let format = |family: PCWSTR, size: f32| -> windows::core::Result<IDWriteTextFormat> {
                let f = dwrite.CreateTextFormat(
                    family,
                    None::<&IDWriteFontCollection>,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size * scale,
                    w!("pt-BR"),
                )?;
                f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                Ok(f)
            };
            let text_fmt = format(family, 12.0)?;
            let icon_fmt = format(w!("Segoe Fluent Icons"), 14.0)?;
            icon_fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;

            let glass_fx = glass::Glass::new(glass_hwnd, 0.0).ok();
            if let Some(fx) = &glass_fx {
                fx.resize(w as f32, h as f32);
            }

            Ok(Self {
                hwnd,
                glass: glass_hwnd,
                _glass_fx: glass_fx,
                scale,
                rect: RECT { left: 0, top: 0, right: w, bottom: h },
                pixmap: Pixmap::new(w as u32, h as u32).expect("pixmap"),
                memdc,
                bits: bits as *mut u8,
                target,
                dwrite,
                text_fmt,
                icon_fmt,
                tray: Vec::new(),
                items: Vec::new(),
                hot: None,
                tracking: false,
                fullscreen: false,
                clock: String::new(),
                battery: status::battery().map(battery_glyph),
                volume: None,
                net: Net::None,
                audio: Audio::new(hwnd),
                wlan: Wlan::open(),
            })
        }
    }

    fn appbar_data(&self) -> APPBARDATA {
        APPBARDATA {
            cbSize: size_of::<APPBARDATA>() as u32,
            hWnd: self.hwnd,
            uCallbackMessage: WM_APPBAR,
            uEdge: ABE_TOP,
            ..Default::default()
        }
    }

    /// Pede a borda de cima ao sistema e põe as duas janelas lá.
    fn place(&mut self) {
        let (w, h) = (self.pixmap.width() as i32, self.pixmap.height() as i32);
        let mut abd = self.appbar_data();
        abd.rc = RECT { left: 0, top: 0, right: w, bottom: h };
        unsafe {
            SHAppBarMessage(ABM_QUERYPOS, &mut abd);
            abd.rc.bottom = abd.rc.top + h;
            SHAppBarMessage(ABM_SETPOS, &mut abd);
        }
        self.rect = RECT { left: abd.rc.left, top: abd.rc.top, right: abd.rc.left + w, bottom: abd.rc.top + h };
        if self.fullscreen {
            return;
        }
        unsafe {
            let _ = SetWindowPos(
                self.glass,
                None,
                self.rect.left,
                self.rect.top,
                w,
                h,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW,
            );
        }
        self.redraw();
    }

    fn set_fullscreen(&mut self, on: bool) {
        if on == self.fullscreen {
            return;
        }
        self.fullscreen = on;
        unsafe {
            if on {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
                let _ = ShowWindow(self.glass, SW_HIDE);
            } else {
                self.place();
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
        }
    }

    /// Relógio agora e timer até a virada do próximo minuto.
    fn update_clock(&mut self) {
        let (text, ms) = status::clock();
        unsafe { SetTimer(Some(self.hwnd), TIMER_CLOCK, ms, None) };
        if text != self.clock {
            self.clock = text;
            self.relayout();
            self.redraw();
        }
    }

    /// Rede e bateria; só redesenha se o ícone mudou.
    fn poll(&mut self) {
        let net = status::network(&self.wlan);
        let battery = status::battery().map(battery_glyph);
        if net != self.net || battery != self.battery {
            self.net = net;
            self.battery = battery;
            self.relayout();
            self.redraw();
        }
    }

    fn update_volume(&mut self) {
        let volume = self.audio.as_ref().and_then(|a| a.state());
        let changed = volume.map(volume_glyph) != self.volume.map(volume_glyph);
        self.volume = volume;
        if changed {
            self.relayout();
            self.redraw();
        }
    }

    fn text_layout(&self, text: &[u16], fmt: &IDWriteTextFormat, w: f32) -> Option<IDWriteTextLayout> {
        let h = self.pixmap.height() as f32 - 1.0;
        unsafe { self.dwrite.CreateTextLayout(text, fmt, w, h).ok() }
    }

    /// Item de glifos (sobrepostos) com a borda direita em `x`; devolve a nova borda.
    fn glyph_item(&self, items: &mut Vec<Item>, x: f32, kind: Kind, glyphs: &[(u16, f32)]) -> f32 {
        let (pad, w) = (PAD * self.scale, GLYPH_W * self.scale);
        let l = x - w - 2.0 * pad;
        let text = glyphs
            .iter()
            .filter_map(|&(g, alpha)| self.text_layout(&[g], &self.icon_fmt, w).map(|t| (t, l + pad, alpha)))
            .collect();
        items.push(Item { kind, l, r: x, text });
        l
    }

    /// Posições dos itens, alinhados à direita (da direita para a esquerda).
    fn relayout(&mut self) {
        let s = self.scale;
        let pad = PAD * s;
        let mut items = Vec::new();
        let mut x = self.pixmap.width() as f32 - 6.0 * s;

        let clock: Vec<u16> = self.clock.encode_utf16().collect();
        if let Some(t) = self.text_layout(&clock, &self.text_fmt, 1000.0) {
            let mut m = DWRITE_TEXT_METRICS::default();
            let _ = unsafe { t.GetMetrics(&mut m) };
            let l = x - m.width.ceil() - 2.0 * pad;
            items.push(Item { kind: Kind::Clock, l, r: x, text: vec![(t, l + pad, 1.0)] });
            x = l;
        }
        if let Some(g) = self.battery {
            x = self.glyph_item(&mut items, x, Kind::Battery, &[(g, 1.0)]);
        }
        if let Some(v) = self.volume {
            x = self.glyph_item(&mut items, x, Kind::Volume, &[(volume_glyph(v), 1.0)]);
        }
        x = self.glyph_item(&mut items, x, Kind::Network, &net_glyphs(self.net));

        // Bandeja: à esquerda dos ícones de status, índice 0 mais à esquerda.
        x -= 4.0 * s;
        for (i, icon) in self.tray.iter().enumerate().rev() {
            let l = x - icon.width() as f32 - 2.0 * TRAY_PAD * s;
            items.push(Item { kind: Kind::Tray(i), l, r: x, text: Vec::new() });
            x = l;
        }
        self.items = items;
    }

    fn hit(&self, x: f32) -> Option<Kind> {
        self.items.iter().find(|i| x >= i.l && x < i.r).map(|i| i.kind)
    }

    fn redraw(&mut self) {
        if self.fullscreen {
            return;
        }
        let light = crate::light_theme();
        let (tint, line, hover, ink) = if light {
            ([246, 246, 248, 150], [0, 0, 0, 30], [0, 0, 0, 24], [0.0, 0.0, 0.0, 0.88])
        } else {
            ([28, 28, 32, 150], [255, 255, 255, 24], [255, 255, 255, 36], [1.0, 1.0, 1.0, 0.95])
        };
        let s = self.scale;
        let (w, h) = (self.pixmap.width() as f32, self.pixmap.height() as f32);
        let id = Transform::identity();

        // Vidro: o desfoque vem da janela de fundo; aqui a tinta, a linha de baixo e o realce.
        let pm = &mut self.pixmap;
        pm.fill(Color::TRANSPARENT);
        if let Some(r) = Rect::from_xywh(0.0, 0.0, w, h - 1.0) {
            pm.fill_rect(r, &solid(tint), id, None);
        }
        if let Some(r) = Rect::from_xywh(0.0, h - 1.0, w, 1.0) {
            pm.fill_rect(r, &solid(line), id, None);
        }
        if let Some(it) = self.items.iter().find(|i| Some(i.kind) == self.hot) {
            let m = 2.0 * s;
            if let Some(p) = rounded_rect(it.l, m, it.r - it.l, h - 1.0 - 2.0 * m, 5.0 * s) {
                pm.fill_path(&p, &solid(hover), FillRule::Winding, id, None);
            }
        }
        for it in &self.items {
            if let Kind::Tray(i) = it.kind
                && let Some(icon) = self.tray.get(i)
            {
                let x = ((it.l + it.r - icon.width() as f32) / 2.0).round() as i32;
                let y = ((h - 1.0 - icon.height() as f32) / 2.0).round() as i32;
                pm.draw_pixmap(x, y, icon.as_ref(), &PixmapPaint::default(), id, None);
            }
        }

        // RGBA (tiny-skia) -> BGRA (GDI), ambos com alfa pré-multiplicado.
        let src = self.pixmap.data();
        let dst = unsafe { std::slice::from_raw_parts_mut(self.bits, src.len()) };
        for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
        }

        // Texto e glifos por cima, direto na DIB.
        let (wi, hi) = (w as i32, h as i32);
        unsafe {
            if self.target.BindDC(self.memdc, &RECT { left: 0, top: 0, right: wi, bottom: hi }).is_ok() {
                let color = D2D1_COLOR_F { r: ink[0], g: ink[1], b: ink[2], a: ink[3] };
                if let Ok(brush) = self.target.CreateSolidColorBrush(&color, None) {
                    self.target.BeginDraw();
                    for it in &self.items {
                        for (t, x, alpha) in &it.text {
                            brush.SetOpacity(*alpha);
                            self.target.DrawTextLayout(Vector2 { X: *x, Y: 0.0 }, t, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
                        }
                    }
                    let _ = self.target.EndDraw(None, None);
                }
            }

            let size = SIZE { cx: wi, cy: hi };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let pos = POINT { x: self.rect.left, y: self.rect.top };
            let _ = UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&pos),
                Some(&size),
                Some(self.memdc),
                Some(&POINT::default()),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }

    /// Ponto (tela) abaixo do centro do item, onde menus devem abrir.
    fn anchor(&self, kind: Kind) -> POINT {
        let it = self.items.iter().find(|i| i.kind == kind);
        let cx = it.map_or(0.0, |i| (i.l + i.r) / 2.0);
        POINT { x: self.rect.left + cx.round() as i32, y: self.rect.bottom }
    }
}

/// Glifos da fonte Segoe Fluent Icons.
fn battery_glyph(b: status::Battery) -> u16 {
    let level = ((b.percent as u16 + 5) / 10).min(10);
    match (b.charging, level) {
        (true, l) => 0xEBAB + l,  // BatteryCharging0..10
        (false, 10) => 0xE83F,    // Battery10
        (false, l) => 0xE850 + l, // Battery0..9
    }
}

fn volume_glyph((level, muted): (f32, bool)) -> u16 {
    match level {
        _ if muted => 0xE74F,
        l if l <= 0.0 => 0xE992,
        l if l < 0.34 => 0xE993,
        l if l < 0.67 => 0xE994,
        _ => 0xE995,
    }
}

/// Wi-Fi: o leque inteiro apagado e, por cima, o nível do sinal.
fn net_glyphs(net: Net) -> Vec<(u16, f32)> {
    match net {
        Net::None => vec![(0xF384, 1.0)],
        Net::Ethernet => vec![(0xE839, 1.0)],
        Net::Wifi(3) => vec![(0xE701, 1.0)],
        Net::Wifi(l) => vec![(0xE701, 0.3), (0xE872 + l as u16, 1.0)],
    }
}

fn solid([r, g, b, a]: [u8; 4]) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(r, g, b, a);
    p.anti_alias = true;
    p
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w / 2.0).min(h / 2.0);
    // Constante de Bézier para quartos de círculo.
    let k = r * 0.5523;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

/// Win + tecla (Win+A: Configurações rápidas; Win+N: notificações e calendário).
fn win_shortcut(key: u8) {
    let input = |vk: VIRTUAL_KEY, flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, dwFlags: flags, ..Default::default() } },
    };
    let (down, up, k) = (KEYBD_EVENT_FLAGS(0), KEYEVENTF_KEYUP, VIRTUAL_KEY(key as u16));
    unsafe {
        SendInput(&[input(VK_LWIN, down), input(k, down), input(k, up), input(VK_LWIN, up)], size_of::<INPUT>() as i32);
    }
}

fn with_bar<R>(f: impl FnOnce(&mut Bar) -> R) -> Option<R> {
    // try_borrow_mut: chamadas do Windows podem reentrar no wndproc.
    BAR.with(|b| b.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

/// Registra quem trata os cliques nos ícones da bandeja.
pub fn on_tray_click(f: TrayClick) {
    TRAY_CLICK.with(|c| c.set(Some(f)));
}

/// Troca os ícones da bandeja (já no tamanho certo, ~16 px × escala) e redesenha.
pub fn set_tray(icons: Vec<Pixmap>) {
    with_bar(|b| {
        b.tray = icons;
        if matches!(b.hot, Some(Kind::Tray(i)) if i >= b.tray.len()) {
            b.hot = None;
        }
        b.relayout();
        b.redraw();
    });
}

/// Item da bandeja sob `x` e o ponto de menu dele.
fn tray_hit(lp: LPARAM) -> Option<(usize, POINT)> {
    with_bar(|b| match b.hit(mouse_x(lp)) {
        Some(k @ Kind::Tray(i)) => Some((i, b.anchor(k))),
        _ => None,
    })
    .flatten()
}

fn tray_click(lp: LPARAM, button: TrayButton) {
    // Fora do borrow: o callback pode chamar set_tray.
    if let Some((i, anchor)) = tray_hit(lp)
        && let Some(f) = TRAY_CLICK.with(|c| c.get())
    {
        f(i, button, anchor);
    }
}

fn mouse_x(lp: LPARAM) -> f32 {
    (lp.0 & 0xffff) as i16 as f32
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_MOUSEMOVE => {
                with_bar(|b| {
                    if !b.tracking {
                        b.tracking = true;
                        let mut tme = TRACKMOUSEEVENT {
                            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        let _ = TrackMouseEvent(&mut tme);
                    }
                    let hot = b.hit(mouse_x(lp));
                    if hot != b.hot {
                        b.hot = hot;
                        b.redraw();
                    }
                });
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                with_bar(|b| {
                    b.tracking = false;
                    if b.hot.take().is_some() {
                        b.redraw();
                    }
                });
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                match with_bar(|b| b.hit(mouse_x(lp))).flatten() {
                    Some(Kind::Network | Kind::Volume | Kind::Battery) => win_shortcut(b'A'),
                    Some(Kind::Clock) => win_shortcut(b'N'),
                    Some(Kind::Tray(_)) => tray_click(lp, TrayButton::Left),
                    None => {}
                }
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                tray_click(lp, TrayButton::Right);
                LRESULT(0)
            }
            WM_LBUTTONDBLCLK => {
                tray_click(lp, TrayButton::Double);
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                // Coordenadas de tela; ±2% por passo da roda sobre o volume.
                let x = (lp.0 & 0xffff) as i16 as i32;
                let steps = ((wp.0 >> 16) as u16 as i16) as f32 / WHEEL_DELTA as f32;
                with_bar(|b| {
                    if b.hit((x - b.rect.left) as f32) == Some(Kind::Volume)
                        && let (Some(a), Some((level, _))) = (&b.audio, b.volume)
                    {
                        a.set_level(level + 0.02 * steps);
                        b.update_volume();
                    }
                });
                LRESULT(0)
            }
            WM_TIMER => {
                with_bar(|b| match wp.0 {
                    TIMER_CLOCK => b.update_clock(),
                    TIMER_POLL => b.poll(),
                    _ => {}
                });
                LRESULT(0)
            }
            status::WM_AUDIO => {
                with_bar(|b| {
                    if wp.0 == 1
                        && let Some(a) = &mut b.audio
                    {
                        a.bind();
                    }
                    b.update_volume();
                });
                LRESULT(0)
            }
            WM_APPBAR => {
                with_bar(|b| match wp.0 as u32 {
                    ABN_POSCHANGED => b.place(),
                    ABN_FULLSCREENAPP => b.set_fullscreen(lp.0 != 0),
                    _ => {}
                });
                LRESULT(0)
            }
            WM_SETTINGCHANGE => {
                // Pode ser a troca de tema claro/escuro.
                with_bar(|b| b.redraw());
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_TIMECHANGE => {
                with_bar(|b| b.update_clock());
                LRESULT(0)
            }
            WM_POWERBROADCAST => {
                // Volta da suspensão ou mudança na energia: relógio e bateria na hora.
                with_bar(|b| {
                    b.update_clock();
                    b.poll();
                });
                LRESULT(1)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// Cria a topbar (depois do dock: reaproveita a classe da janela de vidro dele).
pub fn create() {
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return };
        let hinst: HINSTANCE = module.into();
        let ex = WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE;

        // Vidro: sem superfície GDI, o conteúdo vem só da composição (glass.rs).
        let Ok(glass) = CreateWindowExW(
            ex | WS_EX_NOREDIRECTIONBITMAP,
            w!("ArcDockWinGlass"),
            w!("ArcDockWinTopbarGlass"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst),
            None,
        ) else {
            return;
        };

        // Conteúdo: janela em camadas, "dona" pelo vidro para ficar sempre à frente dele.
        let class = w!("ArcDockWinTopbar");
        let wc = WNDCLASSW {
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            ex | WS_EX_LAYERED,
            class,
            class,
            WS_POPUP,
            0,
            0,
            0,
            0,
            Some(glass),
            None,
            Some(hinst),
            None,
        ) else {
            return;
        };

        let Ok(mut bar) = Bar::new(hwnd, glass) else {
            let _ = DestroyWindow(glass);
            return;
        };
        // AppBar na borda de cima: janelas maximizadas ficam abaixo da barra.
        let mut abd = bar.appbar_data();
        SHAppBarMessage(ABM_NEW, &mut abd);

        bar.update_clock();
        bar.net = status::network(&bar.wlan);
        bar.volume = bar.audio.as_ref().and_then(|a| a.state());
        bar.relayout();
        bar.place();
        BAR.with(|b| *b.borrow_mut() = Some(bar));

        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), TIMER_POLL, POLL_MS, None);
    }
}

/// Tira a barra do sistema de AppBars (chamado onde o dock encerra).
pub fn shutdown() {
    let Some(bar) = BAR.with(|b| b.try_borrow_mut().ok().and_then(|mut g| g.take())) else {
        return;
    };
    let mut abd = bar.appbar_data();
    unsafe {
        SHAppBarMessage(ABM_REMOVE, &mut abd);
        let _ = ShowWindow(bar.hwnd, SW_HIDE);
        let _ = ShowWindow(bar.glass, SW_HIDE);
    }
}
