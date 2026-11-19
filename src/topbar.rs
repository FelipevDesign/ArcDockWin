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
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::Shell::{
    ABE_TOP, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, ABN_FULLSCREENAPP, ABN_POSCHANGED,
    APPBARDATA, SHAppBarMessage,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, w};
use windows_numerics::Vector2;

use crate::glass;
use crate::panel::{self, PanelContent};
use crate::status::{self, Audio, Net, Wlan};

/// Botão usado num clique de ícone da bandeja.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayButton {
    Left,
    Right,
    Double,
}

/// Ícone da bandeja (já em tons de cinza e no tamanho certo, ~16 px × escala).
#[derive(Clone)]
pub struct TrayEntry {
    pub icon: Pixmap,
    pub tip: String,
    /// Chave no banco (`exe#uid`).
    pub key: String,
    /// Na barra (`true`) ou dentro da bandeja.
    pub shown: bool,
}

/// Callback de clique na bandeja: índice do ícone, botão e ponto (tela) onde menus devem abrir.
pub type TrayClick = fn(index: usize, button: TrayButton, anchor: POINT);

const WM_APPBAR: u32 = WM_APP + 1;
const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_CLOCK: usize = 1;
const TIMER_POLL: usize = 2;
/// Releitura da cor do app maximizado: logo depois de um evento de janela e, enquanto
/// pintada, de tempos em tempos (o app pode mudar de cor sem mexer na janela).
const TIMER_TINT: usize = 3;
const TIMER_STATS: usize = 4;
const STATS_MS: u32 = 2000;
/// Largura (px) de cada fatia do brilho do wallpaper.
const LUMA_BUCKET: usize = 16;
const GLYPH_CPU: u16 = 0xE950;
const GLYPH_RAM: u16 = 0xE964;
const TINT_DEBOUNCE_MS: u32 = 120;
const TINT_REFRESH_MS: u32 = 1500;
/// Segunda leitura antes de gravar a cor de um app novo no banco.
const TINT_CONFIRM_MS: u32 = 400;
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
    /// Seta que abre a bandeja (ícones ocultos e a escolha de quem fica na barra).
    Overflow,
    Network,
    Bluetooth,
    Volume,
    Battery,
    Clock,
    /// CPU e RAM à esquerda: só leitura, sem clique nem realce.
    Stats,
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
    /// Ícones da bandeja (os ocultos ficam na lista, mas fora da barra).
    tray: Vec<TrayEntry>,
    items: Vec<Item>,
    hot: Option<Kind>,
    tracking: bool,
    /// App em tela cheia: a barra sai da frente.
    fullscreen: bool,
    clock: String,
    battery: Option<u16>,
    volume: Option<(f32, bool)>,
    net: Net,
    /// Rádio Bluetooth: None = não existe; Some(ligado).
    bt: Option<bool>,
    /// Cor do app maximizado logo abaixo (RGB); None = vidro normal.
    app_color: Option<[u8; 3]>,
    /// Brilho do wallpaper sob a barra, por fatia de LUMA_BUCKET px (vazio = não lido).
    wall: Vec<f32>,
    usage: status::Usage,
    cpu: Option<u8>,
    ram: Option<u8>,
    /// Primeira leitura de um app ainda sem cor no banco: (app, cor), esperando confirmação.
    tint_pending: Option<(String, [u8; 3])>,
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
                // Sem desfoque: a barra é transparente na área de trabalho e sólida
                // (cor do app) sobre um app maximizado.
                fx.set_opacity(0.0);
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
                app_color: None,
                wall: Vec::new(),
                usage: status::Usage::new(),
                cpu: None,
                ram: None,
                tint_pending: None,
                clock: String::new(),
                battery: status::battery().map(battery_glyph),
                volume: None,
                net: Net::None,
                bt: None,
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
    /// Acha o app maximizado embaixo e pega a cor dele: do banco (colordb) se já houver;
    /// senão lê da tela e grava quando duas leituras seguidas concordam.
    fn update_tint(&mut self) {
        let mut next = 0;
        let color = if panel::any_open() || self.fullscreen {
            // Painel aberto cobre a faixa lida: mantém a cor e tenta de novo depois.
            next = TINT_REFRESH_MS;
            self.app_color
        } else if let crate::tint::Below::Maximized(win) = crate::tint::below(self.rect) {
            let key = crate::tint::app_key(win);
            if let Some(c) = key.as_deref().and_then(crate::colordb::get) {
                self.tint_pending = None;
                Some(c)
            } else {
                let c = crate::tint::read(self.rect);
                match (key, c) {
                    (Some(k), Some(c)) => {
                        if self.tint_pending.as_ref().is_some_and(|(pk, pc)| *pk == k && *pc == c) {
                            crate::colordb::set(&k, c);
                            self.tint_pending = None;
                        } else {
                            self.tint_pending = Some((k, c));
                            next = TINT_CONFIRM_MS;
                        }
                    }
                    // Sem chave (UWP): sem banco, relê de tempos em tempos.
                    (None, Some(_)) => next = TINT_REFRESH_MS,
                    _ => self.tint_pending = None,
                }
                c
            }
        } else {
            self.tint_pending = None;
            // Área de trabalho livre embaixo: relê o brilho do wallpaper (com janela solta
            // encostada, mantém a última leitura).
            if matches!(crate::tint::below(self.rect), crate::tint::Below::Desktop)
                && let Some(l) = crate::tint::wallpaper_luma(self.rect, LUMA_BUCKET)
                && l != self.wall
            {
                self.wall = l;
                if self.app_color.is_none() {
                    self.redraw();
                }
            }
            None
        };
        unsafe {
            if next > 0 {
                SetTimer(Some(self.hwnd), TIMER_TINT, next, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_TINT);
            }
        }
        if color != self.app_color {
            self.app_color = color;
            self.redraw();
        }
    }

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

    fn update_stats(&mut self) {
        let (cpu, ram) = (self.usage.cpu(), self.usage.ram());
        if (cpu, ram) != (self.cpu, self.ram) {
            self.cpu = cpu;
            self.ram = ram;
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

        // CPU e RAM à esquerda: rótulo apagado + valor; o valor tem largura fixa ("100%")
        // para o texto não pular quando o número muda.
        {
            let mut lx = 6.0 * s + pad;
            let mut text = Vec::new();
            let width = |t: &IDWriteTextLayout| {
                let mut m = DWRITE_TEXT_METRICS::default();
                let _ = unsafe { t.GetMetrics(&mut m) };
                m.width.ceil()
            };
            // Largura mínima de "00%": o valor não fica pulando entre 9% e 10%.
            let value_w = self
                .text_layout(&"00%".encode_utf16().collect::<Vec<_>>(), &self.text_fmt, 200.0)
                .map(|t| width(&t))
                .unwrap_or(30.0 * s);
            let gw = GLYPH_W * s;
            for (glyph, v) in [(GLYPH_CPU, self.cpu), (GLYPH_RAM, self.ram)] {
                let value = v.map(|v| format!("{v}%")).unwrap_or_else(|| "–".into());
                if let Some(t) = self.text_layout(&[glyph], &self.icon_fmt, gw) {
                    text.push((t, lx, 0.75));
                    lx += gw + 2.0 * s;
                }
                if let Some(t) = self.text_layout(&value.encode_utf16().collect::<Vec<_>>(), &self.text_fmt, 200.0) {
                    let w = width(&t).max(value_w);
                    text.push((t, lx, 1.0));
                    lx += w + 8.0 * s;
                }
            }
            items.push(Item { kind: Kind::Stats, l: 6.0 * s, r: lx, text });
        }

        // Data e hora no centro da barra.
        let clock: Vec<u16> = self.clock.encode_utf16().collect();
        if let Some(t) = self.text_layout(&clock, &self.text_fmt, 1000.0) {
            let mut m = DWRITE_TEXT_METRICS::default();
            let _ = unsafe { t.GetMetrics(&mut m) };
            let cw = m.width.ceil() + 2.0 * pad;
            let l = ((self.pixmap.width() as f32 - cw) / 2.0).round();
            items.push(Item { kind: Kind::Clock, l, r: l + cw, text: vec![(t, l + pad, 1.0)] });
        }
        if let Some(g) = self.battery {
            x = self.glyph_item(&mut items, x, Kind::Battery, &[(g, 1.0)]);
        }
        if let Some(v) = self.volume {
            x = self.glyph_item(&mut items, x, Kind::Volume, &[(volume_glyph(v), 1.0)]);
        }
        x = self.glyph_item(&mut items, x, Kind::Network, &net_glyphs(self.net));
        if let Some(on) = self.bt {
            x = self.glyph_item(&mut items, x, Kind::Bluetooth, &[(panel::glyph::BLUETOOTH, if on { 1.0 } else { 0.35 })]);
        }

        // Bandeja: à esquerda dos ícones de status, índice 0 mais à esquerda.
        x -= 4.0 * s;
        for (i, e) in self.tray.iter().enumerate().rev().filter(|(_, e)| e.shown) {
            let l = x - e.icon.width() as f32 - 2.0 * TRAY_PAD * s;
            items.push(Item { kind: Kind::Tray(i), l, r: x, text: Vec::new() });
            x = l;
        }
        if !self.tray.is_empty() {
            self.glyph_item(&mut items, x, Kind::Overflow, &[(panel::glyph::CHEVRON_DOWN, 0.8)]);
        }
        self.items = items;
    }

    /// Se o wallpaper sob [l, r) é claro (texto escuro); None se ainda não foi lido.
    fn wall_light(&self, l: f32, r: f32) -> Option<bool> {
        let a = (l.max(0.0) as usize / LUMA_BUCKET).min(self.wall.len().checked_sub(1)?);
        let b = ((r.max(0.0) as usize).div_ceil(LUMA_BUCKET)).clamp(a + 1, self.wall.len());
        let avg = self.wall[a..b].iter().sum::<f32>() / (b - a) as f32;
        Some(avg > 150.0)
    }

    fn hit(&self, x: f32) -> Option<Kind> {
        self.items.iter().find(|i| x >= i.l && x < i.r && i.kind != Kind::Stats).map(|i| i.kind)
    }

    fn redraw(&mut self) {
        if self.fullscreen {
            return;
        }
        // Com app maximizado embaixo: fundo sólido na cor dele, sem linha, tinta pelo contraste.
        let app = self.app_color;
        let light = match app {
            Some([r, g, b]) => 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32 > 150.0,
            None => crate::light_theme(),
        };
        let ink = if light { [0.0, 0.0, 0.0, 0.88] } else { [1.0, 1.0, 1.0, 0.95] };
        // Sobre o app: fundo sólido na cor dele. Área de trabalho: transparente
        // (alfa 1 para continuar recebendo cliques).
        let tint = match app {
            Some([r, g, b]) => [r, g, b, 255],
            None => [0, 0, 0, 1],
        };
        let line = tint;
        // Tinta de cada item: pela cor do app ou pelo brilho do wallpaper sob o item.
        let item_light: Vec<bool> = self
            .items
            .iter()
            .map(|it| match app {
                Some(_) => light,
                None => self.wall_light(it.l, it.r).unwrap_or(light),
            })
            .collect();
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
        if let Some((i, it)) = self.items.iter().enumerate().find(|(_, i)| Some(i.kind) == self.hot) {
            let hover = if item_light[i] { [0, 0, 0, 24] } else { [255, 255, 255, 36] };
            let m = 2.0 * s;
            if let Some(p) = rounded_rect(it.l, m, it.r - it.l, h - 1.0 - 2.0 * m, 5.0 * s) {
                pm.fill_path(&p, &solid(hover), FillRule::Winding, id, None);
            }
        }
        for it in &self.items {
            if let Kind::Tray(i) = it.kind
                && let Some(icon) = self.tray.get(i).map(|e| &e.icon)
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
                    for (it, &il) in self.items.iter().zip(&item_light) {
                        let c = if app.is_some() || il == light {
                            color
                        } else if il {
                            D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.88 }
                        } else {
                            D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 0.95 }
                        };
                        brush.SetColor(&c);
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

    /// Retângulo do item na tela (altura inteira da barra).
    fn item_rect(&self, kind: Kind) -> Option<RECT> {
        let it = self.items.iter().find(|i| i.kind == kind)?;
        Some(RECT {
            left: self.rect.left + it.l.round() as i32,
            top: self.rect.top,
            right: self.rect.left + it.r.round() as i32,
            bottom: self.rect.bottom,
        })
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

pub(crate) fn volume_glyph((level, muted): (f32, bool)) -> u16 {
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

pub(crate) fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
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

/// Painel de cada item de status: chave (para o segundo clique fechar) e o conteúdo.
/// É aqui que se pluga um painel novo (ex.: Wi‑Fi em `Kind::Network`).
fn panel_for(kind: Kind) -> Option<(&'static str, fn() -> Box<dyn PanelContent>)> {
    Some(match kind {
        Kind::Clock => ("clock", || Box::new(crate::calendar::Calendar::new())),
        Kind::Volume => ("volume", || Box::new(crate::volume::VolumePanel::new())),
        Kind::Network => ("network", || Box::new(crate::wifi::WifiPanel::new())),
        Kind::Bluetooth => ("bluetooth", || Box::new(crate::bluetooth::BluetoothPanel::new())),
        Kind::Battery => ("battery", || Box::new(panel::Placeholder("Bateria"))),
        Kind::Overflow => ("tray", || Box::new(crate::overflow::Overflow::new())),
        Kind::Tray(_) | Kind::Stats => return None,
    })
}

/// Janela da topbar (destino das mensagens de trabalho feito em outra thread).
pub fn hwnd() -> HWND {
    with_bar(|b| b.hwnd).unwrap_or_default()
}

fn with_bar<R>(f: impl FnOnce(&mut Bar) -> R) -> Option<R> {
    // try_borrow_mut: chamadas do Windows podem reentrar no wndproc.
    BAR.with(|b| b.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

/// Registra quem trata os cliques nos ícones da bandeja.
pub fn on_tray_click(f: TrayClick) {
    TRAY_CLICK.with(|c| c.set(Some(f)));
}

/// Troca os ícones da bandeja e redesenha.
pub fn set_tray(icons: Vec<TrayEntry>) {
    with_bar(|b| {
        b.tray = icons;
        if matches!(b.hot, Some(Kind::Tray(i)) if i >= b.tray.len()) {
            b.hot = None;
        }
        b.relayout();
        b.redraw();
    });
    // Fora do borrow da barra: o painel lê a lista de novo.
    if panel::is_open("tray") {
        panel::refresh();
    }
}

/// Ícones da bandeja, na ordem dos índices de clique.
pub fn tray_list() -> Vec<TrayEntry> {
    with_bar(|b| b.tray.clone()).unwrap_or_default()
}

/// Põe o ícone `i` na barra (`true`) ou dentro da bandeja, e guarda a escolha.
pub fn set_tray_shown(i: usize, shown: bool) {
    with_bar(|b| {
        let Some(e) = b.tray.get_mut(i) else { return };
        e.shown = shown;
        crate::colordb::set_tray_shown(&e.key, shown);
        b.relayout();
        b.redraw();
    });
}

/// Clique no ícone `i` vindo de fora da barra (a bandeja); o menu abre no cursor.
pub fn tray_click_index(i: usize, button: TrayButton) {
    let mut anchor = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut anchor);
    }
    if let Some(f) = TRAY_CLICK.with(|c| c.get()) {
        f(i, button, anchor);
    }
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
        panel::close();
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
                let hit = with_bar(|b| b.hit(mouse_x(lp)).map(|k| (k, b.item_rect(k)))).flatten();
                match hit {
                    Some((Kind::Tray(_), _)) => tray_click(lp, TrayButton::Left),
                    // Fora do borrow da barra: abrir o painel ativa outra janela.
                    Some((kind, Some(rect))) => {
                        if let Some((key, make)) = panel_for(kind) {
                            panel::toggle(key, rect, make);
                        }
                    }
                    _ => {}
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
                    TIMER_TINT => b.update_tint(),
                    TIMER_STATS => b.update_stats(),
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
                panel::notify(msg, wp.0);
                LRESULT(0)
            }
            crate::bluetooth::WM_BT_RADIO | crate::bluetooth::WM_BT_PANEL => {
                if msg == crate::bluetooth::WM_BT_RADIO {
                    with_bar(|b| {
                        let bt = Some(wp.0 == 2);
                        if bt != b.bt {
                            b.bt = bt;
                            b.relayout();
                            b.redraw();
                        }
                    });
                }
                panel::notify(msg, wp.0);
                LRESULT(0)
            }
            crate::wifi::WM_WIFI => {
                panel::notify(msg, wp.0);
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
                // Pode ser a troca de tema claro/escuro ou de wallpaper.
                with_bar(|b| {
                    b.redraw();
                    SetTimer(Some(hwnd), TIMER_TINT, 300, None);
                });
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
        SetTimer(Some(hwnd), TIMER_STATS, STATS_MS, None);
        crate::bluetooth::watch(hwnd);
        // Eventos de janela (foco, minimizar, mover, maximizar) relêem a cor do app.
        let _ = SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, None, Some(on_window_event), 0, 0, WINEVENT_OUTOFCONTEXT);
        let _ = SetWinEventHook(EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, None, Some(on_window_event), 0, 0, WINEVENT_OUTOFCONTEXT);
        let _ = SetWinEventHook(EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE, None, Some(on_window_event), 0, 0, WINEVENT_OUTOFCONTEXT);
        SetTimer(Some(hwnd), TIMER_TINT, TINT_DEBOUNCE_MS, None);
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

/// Algo mudou numa janela de topo: agenda a releitura da cor (o timer junta rajadas).
unsafe extern "system" fn on_window_event(
    _: HWINEVENTHOOK,
    _: u32,
    hwnd: HWND,
    obj: i32,
    child: i32,
    _: u32,
    _: u32,
) {
    if hwnd.is_invalid() || obj != OBJID_WINDOW.0 || child != CHILDID_SELF as i32 {
        return;
    }
    let bar = BAR.with(|b| b.try_borrow().ok().and_then(|g| g.as_ref().map(|b| (b.hwnd, b.glass))));
    if let Some((h, g)) = bar
        && hwnd != h
        && hwnd != g
    {
        unsafe { SetTimer(Some(h), TIMER_TINT, TINT_DEBOUNCE_MS, None) };
    }
}
