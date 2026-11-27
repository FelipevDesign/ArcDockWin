//! Topbar: barra fina no topo de cada monitor, estilo barra de menus do macOS.
//! Mesmo padrão do dock: janela de vidro atrás + janela em camadas por cima. A tinta, a
//! linha de baixo, o realce e os ícones da bandeja saem do tiny-skia; texto e glifos de
//! status são desenhados com DirectWrite (Direct2D ligado ao memdc da DIB).

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::time::{Duration, Instant};

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
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory,
    IDWriteFontCollection, IDWriteTextFormat, IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, EnumDisplayMonitors,
    GetMonitorInfoW, HBITMAP, HDC, HGDIOBJ, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromWindow, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::Shell::{
    ABE_TOP, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, ABN_FULLSCREENAPP, ABN_POSCHANGED,
    APPBARDATA, SHAppBarMessage,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, HSTRING, PCWSTR, w};
use windows_numerics::Vector2;

use crate::config::{self, Module};
use crate::glass;
use crate::panel::{self, PanelContent};
use crate::status::{self, Audio};
use crate::usage::Provider;

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
    /// Preferência do usuário: na barra (`true`) ou dentro da bandeja.
    pub shown: bool,
    /// Na barra de fato: `shown` menos os recolhidos por falta de espaço (refeito em `relayout`).
    pub visible: bool,
}

/// Callback de clique na bandeja: índice do ícone, botão e ponto (tela) onde menus devem abrir.
pub type TrayClick = fn(index: usize, button: TrayButton, anchor: POINT);

const WM_APPBAR: u32 = WM_APP + 1;
const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_CLOCK: usize = 1;
/// Releitura da cor do app maximizado: logo depois de um evento de janela e, enquanto
/// pintada, de tempos em tempos (o app pode mudar de cor sem mexer na janela).
const TIMER_TINT: usize = 3;
/// Relê CPU/RAM (esquerda) e avisa a Central de Controle aberta.
const TIMER_STATS: usize = 4;
/// Releitura depois de eventos de janela: no máximo uma a cada TINT_THROTTLE_MS
/// (não é reiniciada a cada evento, senão uma rajada adiaria a cor até acabar).
const TIMER_TINT_EVENT: usize = 5;
const TINT_THROTTLE_MS: u32 = 50;
/// Monitores mudaram (conectar, resolução, escala): recria as barras depois de um respiro.
const TIMER_REBUILD: usize = 6;
const REBUILD_MS: u32 = 300;
const STATS_MS: u32 = 2000;
/// Relê o uso do Claude e do Codex (só com o módulo ligado).
const TIMER_USAGE: usize = 7;
/// Relê o layout de teclado do app em foco: o Win+Espaço e o Alt+Shift não avisam ninguém.
const TIMER_KEYBOARD: usize = 8;
const KEYBOARD_MS: u32 = 400;
/// Avisos, clima e agenda: um tique a cada 30 s (clima a cada 30 min, agenda a cada 15).
const TIMER_SLOW: usize = 9;
const SLOW_MS: u32 = 30_000;
/// Barra que se esconde: vigia o mouse no topo da tela.
const TIMER_AUTOHIDE: usize = 10;
const AUTOHIDE_MS: u32 = 120;
/// Largura (px) de cada fatia do brilho do wallpaper.
const LUMA_BUCKET: usize = 16;
pub(crate) const GLYPH_CPU: u16 = 0xE950;
const GLYPH_RAM: u16 = 0xE964;
const TINT_DEBOUNCE_MS: u32 = 120;
const TINT_REFRESH_MS: u32 = 1500;
/// Segunda leitura antes de gravar a cor de um app novo no banco.
const TINT_CONFIRM_MS: u32 = 400;
/// Wallpaper sob a barra: relido a cada evento durante WALL_SETTLE depois de a área ficar
/// livre; depois, no máximo a cada WALL_REFRESH.
const WALL_SETTLE: Duration = Duration::from_millis(1000);
const WALL_REFRESH: Duration = Duration::from_secs(5);

/// Altura da barra, espaço interno de cada item e largura de um glifo (px antes da escala).
const HEIGHT: f32 = 24.0;
const PAD: f32 = 6.0;
const GLYPH_W: f32 = 16.0;
const TRAY_PAD: f32 = 4.0;

thread_local! {
    /// Uma barra por monitor; a primeira é a do monitor principal.
    static BARS: RefCell<Vec<Bar>> = const { RefCell::new(Vec::new()) };
    /// Janela da barra principal: nunca muda (destino de mensagens de outros módulos).
    static MAIN: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    /// Som e CPU/RAM são um só para todas as barras.
    static AUDIO: RefCell<Option<Audio>> = const { RefCell::new(None) };
    static USAGE: RefCell<Option<status::Usage>> = const { RefCell::new(None) };
    static TRAY_CLICK: Cell<Option<TrayClick>> = const { Cell::new(None) };
    /// Barra cuja seta abriu a bandeja: cada barra recolhe ícones conforme a própria largura.
    static TRAY_BAR: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    /// Ganchos de eventos de janela da barra (tirados em `destroy`).
    static HOOKS: RefCell<Vec<HWINEVENTHOOK>> = const { RefCell::new(Vec::new()) };
    /// Sigla do layout de teclado atual ("PT", "EN"; vazia com um layout só), igual em
    /// todas as barras, e o (layout, nº de layouts) de que ela saiu.
    static KEYBOARD: RefCell<String> = const { RefCell::new(String::new()) };
    static KEYBOARD_SEEN: Cell<(isize, usize)> = const { Cell::new((0, 0)) };
}

/// A barra existe (foi criada e não foi desligada nas Configurações).
fn active() -> bool {
    !MAIN.get().is_invalid()
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Tray(usize),
    /// Seta que abre a bandeja (ícones ocultos e a escolha de quem fica na barra).
    Overflow,
    Clock,
    /// Central de Controle (último à direita): rede, Bluetooth, som, energia, mídia.
    Control,
    /// CPU e RAM à esquerda; o clique abre o Gerenciador de Tarefas.
    Stats,
    /// Uso do Claude ou do Codex (% da sessão de 5 h); o clique abre o painel de uso.
    Usage(crate::usage::Provider),
    /// Mostrar a área de trabalho (como o canto da barra de tarefas do Windows): o clique
    /// faz o Win+D, e o segundo clique traz as janelas de volta.
    Desktop,
    /// Idioma de entrada (layout de teclado): o clique abre a lista para trocar e a roda passa
    /// para o próximo.
    Keyboard,
    /// Logo do Windows: menu do sistema.
    Menu,
    /// Nome do app em foco: menu do app.
    App,
    /// Área de trabalho virtual: lista; a roda troca.
    Space,
    /// Ferramenta n de `tools::bar_tools()`.
    Tool(u8),
    Shot,
    Weather,
    Event,
    Battery,
    Notify,
}

/// Glifo do botão da área de trabalho (TVMonitor; existe na Fluent Icons e na MDL2 Assets).
const GLYPH_DESKTOP: u16 = 0xE7F4;

/// Item desenhado: faixa horizontal [l, r) e textos (layout, x, opacidade).
struct Item {
    kind: Kind,
    l: f32,
    r: f32,
    text: Vec<(IDWriteTextLayout, f32, f32)>,
}

/// Direct2D em software desenha direto na DIB, por cima do que já está nela, sem dispositivo
/// de GPU (96 dpi: 1 DIP = 1 px; a escala vai nos tamanhos). Um render target e uma fábrica
/// DirectWrite para todas as barras (mesma thread; BindDC troca a DIB a cada desenho): cada
/// render target em software custa ~2 MB.
pub(crate) fn shared_d2d() -> windows::core::Result<(ID2D1DCRenderTarget, IDWriteFactory)> {
    D2D.with_borrow_mut(|slot| {
        if let Some(pair) = slot {
            return Ok(pair.clone());
        }
        unsafe {
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
            Ok(slot.insert((target, dwrite)).clone())
        }
    })
}

thread_local! {
    static D2D: RefCell<Option<(ID2D1DCRenderTarget, IDWriteFactory)>> = const { RefCell::new(None) };
}

/// Descarta o render target compartilhado; o próximo `shared_d2d` cria outro. Um alvo que
/// falhou no BindDC ou no EndDraw (ex.: D2DERR_RECREATE_TARGET) não volta sozinho: sem
/// isso, o texto de todas as barras (relógio, CPU/RAM, glifos) sumia até reabrir o dock.
fn reset_shared_d2d() {
    D2D.with_borrow_mut(|slot| *slot = None);
}

struct Bar {
    hwnd: HWND,
    /// Janela atrás que só fornece o desfoque (vidro).
    glass: HWND,
    glass_fx: Option<glass::Glass>,
    scale: f32,
    /// Monitor desta barra e o retângulo inteiro dele.
    hmon: HMONITOR,
    monitor: RECT,
    /// Posição atual na tela (dada pelo sistema como AppBar).
    rect: RECT,
    pixmap: Pixmap,
    memdc: HDC,
    dib: HBITMAP,
    bits: *mut u8,
    dwrite: IDWriteFactory,
    text_fmt: IDWriteTextFormat,
    /// Nome do app em foco (semi-negrito).
    bold_fmt: IDWriteTextFormat,
    icon_fmt: IDWriteTextFormat,
    /// Barra que se esconde sozinha está fora da tela agora; e desde quando o mouse saiu.
    autohidden: bool,
    leave_at: Option<Instant>,
    /// Ícones da bandeja (os ocultos ficam na lista, mas fora da barra).
    tray: Vec<TrayEntry>,
    items: Vec<Item>,
    hot: Option<Kind>,
    tracking: bool,
    /// App em tela cheia: a barra sai da frente.
    fullscreen: bool,
    clock: String,
    /// Cor do app maximizado logo abaixo (RGB); None = vidro normal.
    app_color: Option<[u8; 3]>,
    /// Brilho do wallpaper sob a barra, por fatia de LUMA_BUCKET px (vazio = não lido).
    wall: Vec<f32>,
    /// Primeira leitura de um app ainda sem cor no banco: (app, cor), esperando confirmação.
    tint_pending: Option<(String, [u8; 3])>,
    /// Desde quando a área sob a barra está livre (None = coberta ou ainda não vista) e a
    /// última leitura do wallpaper.
    wall_since: Option<Instant>,
    wall_read: Option<Instant>,
    /// Releitura da cor já agendada (TIMER_TINT_EVENT).
    tint_queued: bool,
    cpu: Option<u8>,
    ram: Option<u8>,
}

impl Bar {
    /// `glass_fx`: o vidro de uma barra anterior da mesma janela (só um por janela).
    unsafe fn new(
        hwnd: HWND,
        glass_hwnd: HWND,
        hmon: HMONITOR,
        monitor: RECT,
        glass_fx: Option<glass::Glass>,
    ) -> windows::core::Result<Self> {
        unsafe {
            let scale = monitor_scale(hmon);
            let w = monitor.right - monitor.left;
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

            let (_, dwrite) = shared_d2d()?;
            let mut fonts: Option<IDWriteFontCollection> = None;
            dwrite.GetSystemFontCollection(&mut fonts, false)?;
            let has = |name: PCWSTR| {
                let (mut i, mut exists) = (0u32, BOOL(0));
                fonts.as_ref().is_some_and(|f| f.FindFamilyName(name, &mut i, &mut exists).is_ok() && exists.as_bool())
            };
            let family = if has(w!("Segoe UI Variable Text")) { w!("Segoe UI Variable Text") } else { w!("Segoe UI") };
            // Windows 10 não tem a Segoe Fluent Icons; a MDL2 Assets usa os mesmos códigos.
            let icons = if has(w!("Segoe Fluent Icons")) { w!("Segoe Fluent Icons") } else { w!("Segoe MDL2 Assets") };
            let locale = HSTRING::from(crate::i18n::text_locale());
            let locale_ptr = PCWSTR::from_raw(locale.as_ptr());
            let format = |family: PCWSTR, size: f32| -> windows::core::Result<IDWriteTextFormat> {
                let f = dwrite.CreateTextFormat(
                    family,
                    None::<&IDWriteFontCollection>,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size * scale,
                    locale_ptr,
                )?;
                f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                Ok(f)
            };
            let text_fmt = format(family, 12.0)?;
            let bold_fmt = dwrite.CreateTextFormat(
                family,
                None::<&IDWriteFontCollection>,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                12.0 * scale,
                locale_ptr,
            )?;
            bold_fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            bold_fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let icon_fmt = format(icons, 14.0)?;
            icon_fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;

            let glass_fx = glass_fx.or_else(|| glass::Glass::new(glass_hwnd, 0.0).ok());
            if let Some(fx) = &glass_fx {
                fx.resize(w as f32, h as f32);
                // Sem desfoque: a barra é transparente na área de trabalho e sólida
                // (cor do app) sobre um app maximizado.
                fx.set_opacity(0.0);
            }

            Ok(Self {
                hwnd,
                glass: glass_hwnd,
                glass_fx,
                scale,
                hmon,
                monitor,
                rect: RECT { left: monitor.left, top: monitor.top, right: monitor.right, bottom: monitor.top + h },
                pixmap: Pixmap::new(w as u32, h as u32).expect("pixmap"),
                memdc,
                dib,
                bits: bits as *mut u8,
                dwrite,
                text_fmt,
                bold_fmt,
                icon_fmt,
                autohidden: false,
                leave_at: None,
                tray: Vec::new(),
                items: Vec::new(),
                hot: None,
                tracking: false,
                fullscreen: false,
                app_color: None,
                wall: Vec::new(),
                wall_since: None,
                wall_read: None,
                tint_pending: None,
                tint_queued: false,
                clock: String::new(),
                cpu: None,
                ram: None,
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
        let hidden = panel::any_open() || self.fullscreen;
        let below = if hidden { crate::tint::Below::Window } else { crate::tint::below(self.rect) };
        let color = if hidden {
            // Painel aberto cobre a faixa lida: mantém a cor e tenta de novo depois.
            next = TINT_REFRESH_MS;
            self.app_color
        } else if let crate::tint::Below::Maximized(win) = below {
            self.wall_since = None;
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
            // encostada, mantém a última leitura). Ler a tela custa ~6 ms: a cada evento só
            // logo depois de a área ficar livre (pega o fim das animações); depois, no máximo
            // a cada WALL_REFRESH (arrastar uma janela gera eventos sem parar).
            if matches!(below, crate::tint::Below::Desktop) {
                let now = Instant::now();
                let settling = now - *self.wall_since.get_or_insert(now) < WALL_SETTLE;
                if settling || self.wall_read.is_none_or(|t| now - t >= WALL_REFRESH) {
                    self.wall_read = Some(now);
                    if settling {
                        next = TINT_CONFIRM_MS;
                    }
                    if let Some(l) = crate::tint::wallpaper_luma(self.rect, LUMA_BUCKET)
                        && l != self.wall
                    {
                        self.wall = l;
                        if self.app_color.is_none() {
                            self.redraw();
                        }
                    }
                }
            } else {
                self.wall_since = None;
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
        let m = self.monitor;
        abd.rc = RECT { left: m.left, top: m.top, right: m.left + w, bottom: m.top + h };
        // Barra que se esconde não reserva espaço: as janelas maximizadas vão até o topo.
        let hide = autohide();
        unsafe {
            SHAppBarMessage(ABM_QUERYPOS, &mut abd);
            abd.rc.bottom = abd.rc.top + if hide { 0 } else { h };
            SHAppBarMessage(ABM_SETPOS, &mut abd);
        }
        self.rect = RECT { left: abd.rc.left, top: abd.rc.top, right: abd.rc.left + w, bottom: abd.rc.top + h };
        unsafe {
            if hide {
                SetTimer(Some(self.hwnd), TIMER_AUTOHIDE, AUTOHIDE_MS, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_AUTOHIDE);
                if self.autohidden {
                    self.autohidden = false;
                    if !self.fullscreen {
                        let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                    }
                }
            }
        }
        if self.fullscreen || self.autohidden {
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

    /// Barra que se esconde: aparece com o mouse encostado no topo do monitor e some
    /// um pouco depois de o mouse sair (nunca com um painel dela aberto).
    fn autohide_tick(&mut self) {
        if self.fullscreen {
            return;
        }
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        let m = self.monitor;
        let in_x = pt.x >= m.left && pt.x < m.right;
        if self.autohidden {
            if in_x && pt.y <= m.top + 1 && pt.y >= m.top - 2 {
                self.autohidden = false;
                self.leave_at = None;
                unsafe {
                    let _ = ShowWindow(self.glass, SW_SHOWNOACTIVATE);
                    let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                }
                self.redraw();
            }
            return;
        }
        let over = in_x && pt.y >= self.rect.top - 2 && pt.y < self.rect.bottom + (4.0 * self.scale) as i32;
        let bar = self.rect;
        let open_here = self.items.iter().any(|it| panel_for(it.kind).is_some_and(|(key, _)| panel::is_open_in(key, bar)))
            || panel::is_open_in("clip", bar)
            || panel::is_open_in("timer", bar);
        if over || open_here {
            self.leave_at = None;
            return;
        }
        let since = *self.leave_at.get_or_insert_with(Instant::now);
        if since.elapsed() >= Duration::from_millis(500) {
            self.autohidden = true;
            self.leave_at = None;
            self.hot = None;
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
                let _ = ShowWindow(self.glass, SW_HIDE);
            }
        }
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
                if !self.autohidden {
                    let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                }
            }
        }
    }

    fn set_clock(&mut self, text: &str) {
        if text != self.clock {
            self.clock = text.to_string();
            self.relayout();
            self.redraw();
        }
    }

    fn set_stats(&mut self, cpu: Option<u8>, ram: Option<u8>) {
        if (cpu, ram) != (self.cpu, self.ram) {
            self.cpu = cpu;
            self.ram = ram;
            self.relayout();
            self.redraw();
        }
    }

    fn text_layout(&self, text: &[u16], fmt: &IDWriteTextFormat, w: f32) -> Option<IDWriteTextLayout> {
        let h = self.pixmap.height() as f32 - 1.0;
        unsafe { self.dwrite.CreateTextLayout(text, fmt, w, h).ok() }
    }

    /// Item de glifos (sobrepostos) com a borda esquerda em `x`; devolve a nova borda direita.
    fn glyph_item(&self, items: &mut Vec<Item>, x: f32, kind: Kind, glyphs: &[(u16, f32)]) -> f32 {
        let (pad, w) = (PAD * self.scale, GLYPH_W * self.scale);
        let text = glyphs
            .iter()
            .filter_map(|&(g, alpha)| self.text_layout(&[g], &self.icon_fmt, w).map(|t| (t, x + pad, alpha)))
            .collect();
        let r = x + w + 2.0 * pad;
        items.push(Item { kind, l: x, r, text });
        r
    }

    /// Item só de texto, com o espaço interno dos lados.
    fn text_item(&self, items: &mut Vec<Item>, kind: Kind, text: &str, fmt: &IDWriteTextFormat, alpha: f32) {
        let pad = PAD * self.scale;
        if let Some(t) = self.text_layout(&text.encode_utf16().collect::<Vec<_>>(), fmt, 1000.0) {
            let w = Self::text_width(&t) + 2.0 * pad;
            items.push(Item { kind, l: 0.0, r: w, text: vec![(t, pad, alpha)] });
        }
    }

    /// Largura do texto de um layout.
    fn text_width(t: &IDWriteTextLayout) -> f32 {
        let mut m = DWRITE_TEXT_METRICS::default();
        let _ = unsafe { t.GetMetrics(&mut m) };
        m.width.ceil()
    }

    /// Itens de um módulo a partir de x = 0 (a posição vem depois, em `relayout`).
    /// Módulo novo: um braço aqui.
    fn module_items(&self, m: Module) -> Vec<Item> {
        let s = self.scale;
        let pad = PAD * s;
        let mut items = Vec::new();
        match m {
            // Glifo apagado + valor; o valor tem largura mínima ("00%") para o texto não pular
            // quando o número muda.
            Module::Stats => {
                let mut lx = pad;
                let mut text = Vec::new();
                let value_w = self
                    .text_layout(&"00%".encode_utf16().collect::<Vec<_>>(), &self.text_fmt, 200.0)
                    .map(|t| Self::text_width(&t))
                    .unwrap_or(30.0 * s);
                let gw = GLYPH_W * s;
                for (glyph, v) in [(GLYPH_CPU, self.cpu), (GLYPH_RAM, self.ram)] {
                    let value = v.map(|v| format!("{v}%")).unwrap_or_else(|| "–".into());
                    if let Some(t) = self.text_layout(&[glyph], &self.icon_fmt, gw) {
                        text.push((t, lx, 0.75));
                        lx += gw + 2.0 * s;
                    }
                    if let Some(t) = self.text_layout(&value.encode_utf16().collect::<Vec<_>>(), &self.text_fmt, 200.0) {
                        let w = Self::text_width(&t).max(value_w);
                        text.push((t, lx, 1.0));
                        lx += w + 8.0 * s;
                    }
                }
                items.push(Item { kind: Kind::Stats, l: 0.0, r: lx - 8.0 * s + pad, text });
            }
            Module::Clock => {
                let clock: Vec<u16> = self.clock.encode_utf16().collect();
                if let Some(t) = self.text_layout(&clock, &self.text_fmt, 1000.0) {
                    let cw = Self::text_width(&t) + 2.0 * pad;
                    items.push(Item { kind: Kind::Clock, l: 0.0, r: cw, text: vec![(t, pad, 1.0)] });
                }
            }
            // Seta dos ocultos (sempre: a engrenagem dela escolhe quem fica na barra) e depois
            // os ícones visíveis, índice 0 mais à esquerda.
            Module::Tray if !self.tray.is_empty() => {
                let mut x = self.glyph_item(&mut items, 0.0, Kind::Overflow, &[(panel::glyph::CHEVRON_DOWN, 0.8)]);
                for (i, e) in self.tray.iter().enumerate().filter(|(_, e)| e.visible) {
                    let r = x + e.icon.width() as f32 + 2.0 * TRAY_PAD * s;
                    items.push(Item { kind: Kind::Tray(i), l: x, r, text: Vec::new() });
                    x = r;
                }
            }
            Module::Tray => {}
            Module::Control => {
                self.glyph_item(&mut items, 0.0, Kind::Control, &[(crate::control::GLYPH, 1.0)]);
            }
            Module::Desktop => {
                self.glyph_item(&mut items, 0.0, Kind::Desktop, &[(GLYPH_DESKTOP, 1.0)]);
            }
            // Sigla do layout; sem item com um layout só (como a barra de tarefas do Windows).
            Module::Keyboard => {
                let label: Vec<u16> = KEYBOARD.with_borrow(|k| k.encode_utf16().collect());
                if !label.is_empty()
                    && let Some(t) = self.text_layout(&label, &self.text_fmt, 200.0)
                {
                    let w = Self::text_width(&t) + 2.0 * pad;
                    items.push(Item { kind: Kind::Keyboard, l: 0.0, r: w, text: vec![(t, pad, 1.0)] });
                }
            }
            // Logo do Windows desenhado em `redraw` (quatro quadradinhos).
            Module::Menu => {
                items.push(Item { kind: Kind::Menu, l: 0.0, r: GLYPH_W * s + 2.0 * pad, text: Vec::new() });
            }
            Module::App => {
                if let Some(app) = crate::topextra::focus() {
                    let mut name: String = app.name.chars().take(32).collect();
                    if app.name.chars().count() > 32 {
                        name.push('…');
                    }
                    self.text_item(&mut items, Kind::App, &name, &self.bold_fmt, 1.0);
                }
            }
            Module::Space => {
                if let Some(name) = SPACE.with_borrow(|n| n.clone()) {
                    self.text_item(&mut items, Kind::Space, &name, &self.text_fmt, 0.85);
                }
            }
            Module::Tools => {
                let mut x = 0.0;
                for (i, t) in crate::tools::bar_tools().into_iter().enumerate() {
                    let g = match t {
                        crate::tools::CLIP => 0xE77F,
                        crate::tools::PICKER => 0xE790,
                        _ => 0xE916,
                    };
                    let start = items.len();
                    x = self.glyph_item(&mut items, x, Kind::Tool(i as u8), &[(g, 0.9)]);
                    // Timer correndo: os minutos que faltam ao lado do relógio.
                    if t == crate::tools::TIMER
                        && let Some(m) = crate::tools::timer_minutes()
                        && let Some(tl) = self.text_layout(&format!("{m}′").encode_utf16().collect::<Vec<_>>(), &self.text_fmt, 200.0)
                    {
                        let w = Self::text_width(&tl);
                        let it = &mut items[start];
                        it.text.push((tl, it.r - pad + 2.0 * s, 1.0));
                        it.r += w + 2.0 * s;
                        x = it.r;
                    }
                }
            }
            Module::Shot => {
                self.glyph_item(&mut items, 0.0, Kind::Shot, &[(0xE722, 0.95)]);
            }
            Module::Weather => {
                if let Some(w) = crate::topextra::weather() {
                    let (sym, _) = crate::topextra::weather_text(w.code, w.day);
                    self.text_item(&mut items, Kind::Weather, &format!("{sym} {:.0}°", w.temp), &self.text_fmt, 1.0);
                }
            }
            Module::Event => {
                match crate::topextra::event_label() {
                    Some(label) => self.text_item(&mut items, Kind::Event, &label, &self.text_fmt, 1.0),
                    // Sem link ainda: o ícone da agenda, para configurar no clique.
                    None if !crate::topextra::has_ical() => {
                        self.glyph_item(&mut items, 0.0, Kind::Event, &[(0xE787, 0.8)]);
                    }
                    None => {}
                }
            }
            Module::Battery => {
                if let Some(b) = crate::topextra::battery() {
                    let sb = status::Battery { percent: b.percent, charging: b.charging };
                    let r = self.glyph_item(&mut items, 0.0, Kind::Battery, &[(crate::control::battery_glyph(&sb), 1.0)]);
                    let mut label = format!("{}%", b.percent);
                    if crate::extras::on("bar_battery_time")
                        && let Some(left) = b.left
                    {
                        label = format!("{label} · {}", crate::topextra::duration_text(left));
                    }
                    if let Some(t) = self.text_layout(&label.encode_utf16().collect::<Vec<_>>(), &self.text_fmt, 400.0) {
                        let w = Self::text_width(&t);
                        let it = items.last_mut().unwrap();
                        it.text.push((t, r - pad, 1.0));
                        it.r = r + w;
                    }
                }
            }
            Module::Notify => {
                let dnd = crate::topextra::dnd() == Some(true);
                let g = if dnd { 0xE708 } else { 0xEA8F };
                let r = self.glyph_item(&mut items, 0.0, Kind::Notify, &[(g, 1.0)]);
                if let Some(n) = crate::topextra::notify_count().filter(|&n| n > 0)
                    && let Some(t) = self.text_layout(&n.min(99).to_string().encode_utf16().collect::<Vec<_>>(), &self.text_fmt, 200.0)
                {
                    let w = Self::text_width(&t);
                    let it = items.last_mut().unwrap();
                    it.text.push((t, r - pad, 1.0));
                    it.r = r + w;
                }
            }
            // Logo do provedor (desenhado em `redraw`) + % da sessão de 5 h.
            Module::Claude | Module::Codex => {
                let p = if m == Module::Claude { Provider::Claude } else { Provider::Codex };
                let gw = GLYPH_W * s;
                let mut lx = pad + gw + 2.0 * s;
                let mut text = Vec::new();
                let value = crate::usage::session_percent(p).map(|p| format!("{p}%")).unwrap_or_else(|| "–".into());
                if let Some(t) = self.text_layout(&value.encode_utf16().collect::<Vec<_>>(), &self.text_fmt, 200.0) {
                    lx += Self::text_width(&t);
                    text.push((t, gw + 2.0 * s + pad, 1.0));
                }
                items.push(Item { kind: Kind::Usage(p), l: 0.0, r: lx + pad, text });
            }
        }
        items
    }

    /// Monta a barra pelos módulos das Configurações: a zona esquerda empilha a partir da
    /// margem, a direita encosta na margem direita e o centro fica centralizado como grupo.
    ///
    /// Falta de espaço: como o centro desliza entre as laterais, tudo cabe enquanto
    /// margens + módulos + um `gap` entre módulos vizinhos (inclusive de zonas vizinhas)
    /// <= largura; o espaço da bandeja é o que sobra disso, esteja ela em qualquer zona.
    /// Se faltar, os ícones da bandeja vão para a seta um a um, do maior índice (ponta
    /// direita, longe da seta) para o menor; zerada a bandeja, sai o CPU/RAM se estiver na
    /// mesma zona dela; só então o centro perde módulos, como antes. As preferências
    /// (`shown`, módulos ligados) não mudam: com espaço de volta, tudo reaparece.
    fn relayout(&mut self) {
        let s = self.scale;
        let (margin, gap) = (6.0 * s, 4.0 * s);
        let width = self.pixmap.width() as f32;
        for e in &mut self.tray {
            e.visible = e.shown;
        }
        let slots: Vec<_> = config::modules().into_iter().filter(|s| s.on).collect();
        let tray_zone = slots.iter().find(|s| s.module == Module::Tray).map(|s| s.zone);
        let mut mods: Vec<(config::Zone, Module, Vec<Item>)> = slots
            .iter()
            .map(|s| (s.zone, s.module, self.module_items(s.module)))
            .filter(|m| !m.2.is_empty())
            .collect();
        let mod_w = |it: &[Item]| it.last().map_or(0.0, |i| i.r);
        let mut excess = 2.0 * margin + mods.iter().map(|m| mod_w(&m.2)).sum::<f32>()
            + gap * mods.len().saturating_sub(1) as f32
            - width;
        let mut collapsed = false;
        while excess > 0.0 {
            if let Some(e) = self.tray.iter_mut().rev().find(|e| e.visible).filter(|_| tray_zone.is_some()) {
                e.visible = false;
                excess -= e.icon.width() as f32 + 2.0 * TRAY_PAD * s;
                collapsed = true;
            } else if let Some(i) = mods.iter().position(|m| m.1 == Module::Stats && Some(m.0) == tray_zone) {
                excess -= mod_w(&mods.remove(i).2) + gap;
            } else {
                break;
            }
        }
        if collapsed && let Some(m) = mods.iter_mut().find(|m| m.1 == Module::Tray) {
            m.2 = self.module_items(Module::Tray);
        }
        // Itens de cada módulo, por zona, e a largura do grupo.
        let mut zones: [Vec<Vec<Item>>; 3] = Default::default();
        for (zone, _, items) in mods {
            zones[zone as usize].push(items);
        }
        let group_w = |mods: &[Vec<Item>]| -> f32 {
            let w: f32 = mods.iter().map(|it| it.last().map_or(0.0, |i| i.r)).sum();
            w + gap * mods.len().saturating_sub(1) as f32
        };
        let [left, mut center, right] = zones;
        let left_end = margin + group_w(&left);
        let right_start = width - margin - group_w(&right);
        // Centro: centralizado; se bate nas laterais, desliza para o lado livre e, se ainda
        // não couber, perde módulos do fim até caber.
        let mut cl = 0.0;
        while !center.is_empty() {
            let cw = group_w(&center);
            let (lo, hi) = (if left.is_empty() { margin } else { left_end + gap }, if right.is_empty() { width - margin } else { right_start - gap });
            cl = ((width - cw) / 2.0).round().max(lo).min(hi - cw);
            if cl >= lo {
                break;
            }
            center.pop();
        }
        let mut items = Vec::new();
        for (mods, start) in [(left, margin), (center, cl), (right, right_start)] {
            let mut x = start.round();
            for mut m in mods {
                let w = m.last().map_or(0.0, |i| i.r);
                for it in &mut m {
                    it.l += x;
                    it.r += x;
                    it.text.iter_mut().for_each(|t| t.1 += x);
                }
                items.extend(m);
                x += w + gap;
            }
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
        self.items.iter().find(|i| x >= i.l && x < i.r).map(|i| i.kind)
    }

    /// Fundo da barra pelo estilo das Configurações: (tinta, linha de baixo, texto escuro
    /// fixo pelo fundo, desfoque, raio dos cantos de baixo).
    fn look(&self) -> (Option<[u8; 3]>, [u8; 4], Option<[u8; 4]>, Option<bool>, bool, f32) {
        let lum = |[r, g, b]: [u8; 3]| 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
        let dyn_line = crate::extras::on("bar_dyncolor").then(|| DYN_COLOR.get()).flatten().map(|[r, g, b]| [r, g, b, 230]);
        let style = crate::extras::choice("bar_style");
        let theme = crate::render::Theme::new(crate::light_theme(), None);
        match style {
            1 if !theme.minimal => {
                let (tint, border) = theme.background();
                (None, tint, Some(dyn_line.unwrap_or(border)), Some(theme.light), true, 0.0)
            }
            2 => {
                let c = crate::extras::bar_color_rgb(crate::extras::choice("bar_color"));
                let a = (crate::extras::num("bar_opacity") / 100.0 * 255.0).round().max(1.0) as u8;
                let light = lum(c) > 150.0;
                let line = crate::extras::on("bar_line").then_some(if light { [0, 0, 0, 40] } else { [255, 255, 255, 46] });
                let fixed = (a >= 100).then_some(light);
                let r = crate::extras::num("bar_radius") * self.scale;
                (None, [c[0], c[1], c[2], a], dyn_line.or(line), fixed, crate::extras::on("bar_blur"), r)
            }
            // Transparente: sobre app maximizado, a cor dele (10% mais escura, para separar).
            _ => {
                let app = self.app_color.map(|c| c.map(|v| (v as f32 * 0.9).round() as u8));
                match app {
                    Some(c) => (Some(c), [c[0], c[1], c[2], 255], Some(dyn_line.unwrap_or([c[0], c[1], c[2], 255])), Some(lum(c) > 150.0), false, 0.0),
                    // Alfa 1 para continuar recebendo cliques.
                    None => (None, [0, 0, 0, 1], dyn_line, None, false, 0.0),
                }
            }
        }
    }

    fn redraw(&mut self) {
        if self.fullscreen || self.autohidden {
            return;
        }
        let (app, tint, line, fixed, blur, radius) = self.look();
        let mut light = fixed.unwrap_or_else(crate::light_theme);
        // Cor do texto escolhida nas Configurações vale mais que o fundo.
        let forced = match crate::extras::choice("bar_ink") {
            1 => Some(false),
            2 => Some(true),
            _ => None,
        };
        if let Some(f) = forced {
            light = f;
        }
        let ink = if light { [0.0, 0.0, 0.0, 0.88] } else { [1.0, 1.0, 1.0, 0.95] };
        // Tinta de cada item: pelo fundo ou pelo brilho do wallpaper sob o item.
        let item_light: Vec<bool> = self
            .items
            .iter()
            .map(|it| match (forced, fixed) {
                (Some(f), _) | (None, Some(f)) => f,
                _ => self.wall_light(it.l, it.r).unwrap_or(light),
            })
            .collect();
        let s = self.scale;
        let (w, h) = (self.pixmap.width() as f32, self.pixmap.height() as f32);
        let id = Transform::identity();
        if let Some(fx) = &self.glass_fx {
            fx.set_opacity(if blur { 1.0 } else { 0.0 });
            fx.bottom_round(w, h, radius);
        }

        // Vidro: o desfoque vem da janela de fundo; aqui a tinta, a linha de baixo e o realce.
        let pm = &mut self.pixmap;
        pm.fill(Color::TRANSPARENT);
        if radius > 0.0 {
            if let Some(p) = bottom_rounded(w, h, radius) {
                pm.fill_path(&p, &solid(tint), FillRule::Winding, id, None);
            }
        } else if let Some(r) = Rect::from_xywh(0.0, 0.0, w, h - 1.0) {
            pm.fill_rect(r, &solid(tint), id, None);
        }
        let line = line.unwrap_or(tint);
        if radius > 0.0 {
            if let Some(r) = Rect::from_xywh(radius, h - 1.0, w - 2.0 * radius, 1.0) {
                pm.fill_rect(r, &solid(line), id, None);
            }
        } else if let Some(r) = Rect::from_xywh(0.0, h - 1.0, w, 1.0) {
            pm.fill_rect(r, &solid(line), id, None);
        }
        // Item cujo painel está aberto nesta barra fica realçado como no hover.
        let bar = self.rect;
        let active =
            self.items.iter().map(|it| it.kind).find(|&k| panel_for(k).is_some_and(|(key, _)| panel::is_open_in(key, bar)));
        let highlighted = self.hot.or(active);
        if let Some((i, it)) = self.items.iter().enumerate().find(|(_, i)| Some(i.kind) == highlighted) {
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
        // Logo do Windows do menu do sistema: quatro quadradinhos na cor do texto.
        for (i, it) in self.items.iter().enumerate() {
            if it.kind != Kind::Menu {
                continue;
            }
            let color = if item_light[i] { [0, 0, 0, 220] } else { [255, 255, 255, 240] };
            let size = (12.0 * s).round();
            let gap = (1.5 * s).max(1.0).round();
            let q = ((size - gap) / 2.0).round();
            let x0 = ((it.l + it.r - size) / 2.0).round();
            let y0 = ((h - 1.0 - size) / 2.0).round();
            for (dx, dy) in [(0.0, 0.0), (q + gap, 0.0), (0.0, q + gap), (q + gap, q + gap)] {
                if let Some(r) = Rect::from_xywh(x0 + dx, y0 + dy, q, q) {
                    pm.fill_rect(r, &solid(color), id, None);
                }
            }
        }
        // Logos de uso na mesma tinta dos glifos do item (preto ou branco conforme o fundo).
        let _ = app;
        for (i, it) in self.items.iter().enumerate() {
            let Kind::Usage(p) = it.kind else { continue };
            let l = item_light[i];
            // Mesma opacidade dos glifos do CPU/RAM (0,75 da tinta).
            let color = if l { [0, 0, 0, 168] } else { [255, 255, 255, 182] };
            if let Some(logo) = crate::usage::logo(p, (14.0 * s).round() as u32, color) {
                let x = (it.l + PAD * s + (GLYPH_W * s - logo.width() as f32) / 2.0).round() as i32;
                let y = ((h - 1.0 - logo.height() as f32) / 2.0).round() as i32;
                pm.draw_pixmap(x, y, logo.as_ref(), &PixmapPaint::default(), id, None);
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
        let shadow = crate::extras::on("bar_shadow");
        unsafe {
            let color = D2D1_COLOR_F { r: ink[0], g: ink[1], b: ink[2], a: ink[3] };
            let draw_text = |target: &ID2D1DCRenderTarget| -> bool {
                if target.BindDC(self.memdc, &RECT { left: 0, top: 0, right: wi, bottom: hi }).is_err() {
                    return false;
                }
                let Ok(brush) = target.CreateSolidColorBrush(&color, None) else { return false };
                target.BeginDraw();
                for (it, &il) in self.items.iter().zip(&item_light) {
                    let c = if il == light {
                        color
                    } else if il {
                        D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.88 }
                    } else {
                        D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 0.95 }
                    };
                    // Sombra leve: 1 px abaixo, escura sob texto claro e clara sob texto escuro.
                    if shadow {
                        let sc = if c.r > 0.5 {
                            D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.55 }
                        } else {
                            D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 0.45 }
                        };
                        brush.SetColor(&sc);
                        for (t, x, alpha) in &it.text {
                            brush.SetOpacity(*alpha);
                            target.DrawTextLayout(Vector2 { X: *x, Y: (s * 1.0).round() }, t, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
                        }
                    }
                    brush.SetColor(&c);
                    for (t, x, alpha) in &it.text {
                        brush.SetOpacity(*alpha);
                        target.DrawTextLayout(Vector2 { X: *x, Y: 0.0 }, t, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    }
                }
                target.EndDraw(None, None).is_ok()
            };
            // Se o alvo falhar, recria e tenta mais uma vez.
            for _ in 0..2 {
                match shared_d2d() {
                    Ok((target, _)) if draw_text(&target) => break,
                    _ => reset_shared_d2d(),
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

impl Drop for Bar {
    /// Barras são refeitas quando os monitores mudam: solta a DIB e o DC.
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.memdc);
            let _ = DeleteObject(HGDIOBJ(self.dib.0));
        }
    }
}

/// Glifo de volume da fonte Segoe Fluent Icons.
pub(crate) fn volume_glyph((level, muted): (f32, bool)) -> u16 {
    match level {
        _ if muted => 0xE74F,
        l if l <= 0.0 => 0xE992,
        l if l < 0.34 => 0xE993,
        l if l < 0.67 => 0xE994,
        _ => 0xE995,
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

/// Retângulo com só os cantos de baixo arredondados.
fn bottom_rounded(w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w / 2.0).min(h);
    let k = r * 0.5523;
    let mut pb = PathBuilder::new();
    pb.move_to(0.0, 0.0);
    pb.line_to(w, 0.0);
    pb.line_to(w, h - r);
    pb.cubic_to(w, h - r + k, w - r + k, h, w - r, h);
    pb.line_to(r, h);
    pb.cubic_to(r - k, h, 0.0, h - r + k, 0.0, h - r);
    pb.close();
    pb.finish()
}

/// Painel de cada item de status: chave (para o segundo clique fechar) e o conteúdo.
/// É aqui que se pluga um painel novo.
fn panel_for(kind: Kind) -> Option<(&'static str, fn() -> Box<dyn PanelContent>)> {
    Some(match kind {
        Kind::Clock => ("clock", || Box::new(crate::calendar::Calendar::new())),
        Kind::Control => ("control", || Box::new(crate::control::ControlCenter::new())),
        Kind::Overflow => ("tray", || Box::new(crate::overflow::Overflow::new())),
        Kind::Stats => ("stats", || Box::new(crate::perf::PerfPanel::new())),
        Kind::Usage(Provider::Claude) => ("claude", || Box::new(crate::usage::UsagePanel::new(Provider::Claude))),
        Kind::Usage(Provider::Codex) => ("codex", || Box::new(crate::usage::UsagePanel::new(Provider::Codex))),
        Kind::Menu => ("sysmenu", || Box::new(crate::topextra::SysMenu::new())),
        Kind::App => ("appmenu", || Box::new(crate::topextra::AppMenu::new())),
        Kind::Space => ("desktops", || Box::new(crate::topextra::DesktopPanel)),
        Kind::Shot => ("shot", || Box::new(crate::topextra::ShotPanel)),
        Kind::Weather => ("weather", || Box::new(crate::topextra::WeatherPanel)),
        Kind::Event => ("agenda", || Box::new(crate::topextra::AgendaPanel::new())),
        Kind::Battery => ("battery", || Box::new(crate::topextra::BatteryPanel)),
        Kind::Tray(_) | Kind::Desktop | Kind::Keyboard | Kind::Tool(_) | Kind::Notify => return None,
    })
}

/// Janela da barra principal (destino das mensagens de trabalho feito em outra thread).
pub fn hwnd() -> HWND {
    MAIN.get()
}

/// Um painel abriu ou fechou: refaz o realce do item dono dele nas barras.
pub fn panel_visibility_changed() {
    for_bars(|b| b.redraw());
}

/// Barra da janela `hwnd`.
fn with_bar<R>(hwnd: HWND, f: impl FnOnce(&mut Bar) -> R) -> Option<R> {
    // try_borrow_mut: chamadas do Windows podem reentrar no wndproc.
    BARS.with(|b| b.try_borrow_mut().ok().and_then(|mut g| g.iter_mut().find(|b| b.hwnd == hwnd).map(f)))
}

/// Todas as barras.
fn for_bars(mut f: impl FnMut(&mut Bar)) {
    BARS.with(|b| {
        if let Ok(mut g) = b.try_borrow_mut() {
            g.iter_mut().for_each(&mut f);
        }
    });
}

/// Relógio agora em todas as barras e timer até a próxima virada (minuto ou segundo).
fn update_clock() {
    let (text, ms) = status::clock();
    unsafe { SetTimer(Some(MAIN.get()), TIMER_CLOCK, ms, None) };
    for_bars(|b| b.set_clock(&text));
    // Compromisso na hora do aviso: bipe e a agenda aberta embaixo dele.
    if config::module_on(Module::Event)
        && let Some(_) = crate::topextra::due_alert()
    {
        unsafe {
            let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(MB_ICONASTERISK);
        }
        if let Some(rect) = item_screen_rect(Module::Event)
            && !panel::is_open("agenda")
        {
            panel::toggle("agenda", rect, || Box::new(crate::topextra::AgendaPanel::new()));
        }
    }
}

thread_local! {
    /// Nome da área de trabalho virtual atual (None = só uma).
    static SPACE: RefCell<Option<String>> = const { RefCell::new(None) };
    /// Cor do ícone do app em foco (linha dinâmica).
    static DYN_COLOR: Cell<Option<[u8; 3]>> = const { Cell::new(None) };
    static SLOW_TICKS: Cell<u32> = const { Cell::new(0) };
    static BATTERY_SEEN: Cell<(u8, bool)> = const { Cell::new((0, false)) };
}

fn autohide() -> bool {
    crate::extras::on("bar_autohide")
}

fn relayout_all() {
    for_bars(|b| {
        b.relayout();
        b.redraw();
    });
}

/// Área de trabalho virtual: relida junto com o teclado (o Windows não avisa a troca).
fn update_space() {
    let name = if config::module_on(Module::Space) {
        crate::topextra::desktops().map(|(names, cur)| names[cur].clone())
    } else {
        None
    };
    if SPACE.with_borrow(|n| *n != name) {
        SPACE.with_borrow_mut(|n| *n = name);
        relayout_all();
    }
}

/// App em foco mudou: nome na barra e a cor da linha dinâmica.
fn update_focus_app() {
    let on = config::module_on(Module::App);
    let dyn_on = crate::extras::on("bar_dyncolor");
    if !on && !dyn_on {
        return;
    }
    if !crate::topextra::update_focus() {
        return;
    }
    if dyn_on {
        let color = crate::topextra::focus()
            .filter(|f| !f.source.is_empty())
            .and_then(|f| crate::apps::load_icon(&f.source, 32))
            .and_then(|icon| crate::render::icon_color(&icon));
        DYN_COLOR.set(color);
    }
    relayout_all();
}

fn update_battery() {
    if !config::module_on(Module::Battery) {
        return;
    }
    let seen = crate::topextra::battery().map_or((0, false), |b| (b.percent, b.charging));
    if seen != BATTERY_SEEN.get() {
        BATTERY_SEEN.set(seen);
        relayout_all();
    }
}

/// A cada 30 s: avisos; clima a cada 30 min; agenda a cada 15 min.
fn slow_tick() {
    let n = SLOW_TICKS.get();
    SLOW_TICKS.set(n.wrapping_add(1));
    if config::module_on(Module::Notify) {
        crate::topextra::refresh_notify();
    }
    if config::module_on(Module::Weather) && (n % 60 == 0 || crate::topextra::weather().is_none() && n % 4 == 0) {
        crate::topextra::refresh_weather();
    }
    if config::module_on(Module::Event) && n % 30 == 0 {
        crate::topextra::refresh_events();
    }
}

/// Ferramentas mudaram (timer andou, ligou ou desligou uma): refaz a barra se elas aparecem.
pub fn tools_changed() {
    if active() && config::module_on(Module::Tools) {
        relayout_all();
    }
}

/// Retângulo (tela) do primeiro item de um módulo na barra principal.
pub fn item_screen_rect(m: Module) -> Option<RECT> {
    let main = MAIN.get();
    with_bar(main, |b| {
        let kind = b.items.iter().map(|i| i.kind).find(|k| {
            matches!(
                (m, k),
                (Module::Menu, Kind::Menu)
                    | (Module::App, Kind::App)
                    | (Module::Event, Kind::Event)
                    | (Module::Weather, Kind::Weather)
                    | (Module::Battery, Kind::Battery)
            )
        })?;
        b.item_rect(kind)
    })
    .flatten()
}

/// Repaint date labels and any open panel after a Windows UI-language change.
pub fn language_changed() {
    if !active() {
        panel::language_changed();
        return;
    }
    update_clock();
    panel::language_changed();
}

/// Módulos ou relógio mudaram nas Configurações: relógio (e o timer dele) e todas as barras.
pub fn settings_changed() {
    if !active() {
        return;
    }
    update_clock();
    update_stats();
    update_usage();
    update_keyboard();
    update_space();
    update_focus_app();
    SLOW_TICKS.set(0);
    slow_tick();
    for_bars(|b| {
        b.place();
        b.relayout();
        b.redraw();
    });
}

fn update_stats() {
    if !config::module_on(Module::Stats) {
        return;
    }
    let Some((cpu, ram)) = USAGE.with(|u| u.borrow_mut().as_mut().map(|u| (u.cpu(), u.ram()))) else { return };
    for_bars(|b| b.set_stats(cpu, ram));
}

/// Relê o layout de teclado do app em foco e, se a sigla mudou, refaz as barras. Barato
/// quando nada mudou (só compara o layout e o nº de layouts).
fn update_keyboard() {
    let on = config::module_on(Module::Keyboard);
    let seen = if on {
        // Janela do dock em foco (o próprio menu de layouts): fica a sigla de antes.
        let Some(hkl) = crate::keyboard::current() else { return };
        (hkl.0 as isize, crate::keyboard::layouts().len())
    } else {
        (0, 0)
    };
    if seen == KEYBOARD_SEEN.get() {
        return;
    }
    KEYBOARD_SEEN.set(seen);
    let label = if on && seen.1 >= 2 {
        crate::keyboard::short_name(windows::Win32::UI::Input::KeyboardAndMouse::HKL(seen.0 as _))
    } else {
        String::new()
    };
    if KEYBOARD.with_borrow(|k| *k == label) {
        return;
    }
    KEYBOARD.with_borrow_mut(|k| *k = label);
    for_bars(|b| {
        b.relayout();
        b.redraw();
    });
}

/// Pede uma releitura do uso do Claude e do Codex; a resposta chega como `usage::WM_USAGE`.
fn update_usage() {
    for (m, p) in [(Module::Claude, Provider::Claude), (Module::Codex, Provider::Codex)] {
        if config::module_on(m) {
            crate::usage::refresh(p, false);
        }
    }
}

/// Registra quem trata os cliques nos ícones da bandeja.
pub fn on_tray_click(f: TrayClick) {
    TRAY_CLICK.with(|c| c.set(Some(f)));
}

/// Troca os ícones da bandeja e redesenha.
pub fn set_tray(icons: Vec<TrayEntry>) {
    for_bars(|b| {
        b.tray = icons.clone();
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

/// Ícones da bandeja, na ordem dos índices de clique; `visible` é o da barra que abriu a
/// seta (ou da principal).
pub fn tray_list() -> Vec<TrayEntry> {
    with_bar(TRAY_BAR.get(), |b| b.tray.clone()).or_else(|| with_bar(MAIN.get(), |b| b.tray.clone())).unwrap_or_default()
}

/// Põe o ícone `i` na barra (`true`) ou dentro da bandeja, e guarda a escolha.
pub fn set_tray_shown(i: usize, shown: bool) {
    let mut key = None;
    for_bars(|b| {
        let Some(e) = b.tray.get_mut(i) else { return };
        e.shown = shown;
        key = Some(e.key.clone());
        b.relayout();
        b.redraw();
    });
    if let Some(k) = key {
        crate::colordb::set_tray_shown(&k, shown);
    }
}

/// Retângulo da barra principal na tela (None sem barra ou durante reentrada).
pub fn main_rect() -> Option<RECT> {
    with_bar(MAIN.get(), |b| b.rect)
}

/// Retângulo na tela do ícone `i` da bandeja, para Shell_NotifyIconGetRect (apps abrem o menu
/// ali). Ícone dentro do painel Bandeja: um quadrado do tamanho da barra em volta do cursor,
/// que acabou de clicar nele no painel. None se a barra está ocupada (reentrada) ou o módulo da
/// bandeja está desligado.
pub fn tray_icon_rect(i: usize) -> Option<RECT> {
    with_bar(MAIN.get(), |b| {
        if let Some(r) = b.item_rect(Kind::Tray(i)) {
            return Some(r);
        }
        b.item_rect(Kind::Overflow)?;
        let mut pt = POINT::default();
        unsafe { GetCursorPos(&mut pt).ok()? };
        let half = (b.rect.bottom - b.rect.top) / 2;
        Some(RECT { left: pt.x - half, top: pt.y - half, right: pt.x + half, bottom: pt.y + half })
    })
    .flatten()
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
fn tray_hit(hwnd: HWND, lp: LPARAM) -> Option<(usize, POINT)> {
    with_bar(hwnd, |b| match b.hit(mouse_x(lp)) {
        Some(k @ Kind::Tray(i)) => Some((i, b.anchor(k))),
        _ => None,
    })
    .flatten()
}

fn tray_click(hwnd: HWND, lp: LPARAM, button: TrayButton) {
    // Fora do borrow: o callback pode chamar set_tray.
    if let Some((i, anchor)) = tray_hit(hwnd, lp)
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
        let main = hwnd == MAIN.get();
        match msg {
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_MOUSEMOVE => {
                with_bar(hwnd, |b| {
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
                with_bar(hwnd, |b| {
                    b.tracking = false;
                    if b.hot.take().is_some() {
                        b.redraw();
                    }
                });
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let hit = with_bar(hwnd, |b| b.hit(mouse_x(lp)).map(|k| (k, b.item_rect(k)))).flatten();
                match hit {
                    Some((Kind::Tray(_), _)) => tray_click(hwnd, lp, TrayButton::Left),
                    Some((Kind::Desktop, _)) => crate::apps::show_desktop(),
                    Some((Kind::Notify, _)) => crate::topextra::open_notifications(),
                    Some((Kind::Tool(i), Some(rect))) => {
                        if let Some(t) = crate::tools::bar_tools().get(i as usize) {
                            crate::tools::bar_click(t, rect);
                        }
                    }
                    Some((Kind::Keyboard, _)) => {
                        // Fora do borrow da barra: o menu roda um laço de mensagens.
                        if let Some(anchor) = with_bar(hwnd, |b| b.anchor(Kind::Keyboard)) {
                            crate::keyboard::menu(hwnd, anchor);
                        }
                        update_keyboard();
                    }
                    // Fora do borrow da barra: abrir o painel ativa outra janela.
                    // O painel abre no monitor do item (rect em coordenadas de tela).
                    Some((kind, Some(rect))) => {
                        if let Some((key, make)) = panel_for(kind) {
                            if kind == Kind::Overflow {
                                TRAY_BAR.set(hwnd);
                            }
                            panel::toggle(key, rect, make);
                        }
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                tray_click(hwnd, lp, TrayButton::Right);
                LRESULT(0)
            }
            WM_LBUTTONDBLCLK => {
                tray_click(hwnd, lp, TrayButton::Double);
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                // Coordenadas de tela; ±2% de volume por passo da roda sobre a central.
                let x = (lp.0 & 0xffff) as i16 as i32;
                let steps = ((wp.0 >> 16) as u16 as i16) as f32 / WHEEL_DELTA as f32;
                let hit = with_bar(hwnd, |b| b.hit((x - b.rect.left) as f32)).flatten();
                if hit == Some(Kind::Space) {
                    // Roda para baixo: área de trabalho da direita; para cima: a da esquerda.
                    crate::topextra::switch_desktop(if steps < 0.0 { 1 } else { -1 });
                }
                if hit == Some(Kind::Keyboard) {
                    // Roda para baixo: próximo layout; para cima: o anterior.
                    crate::keyboard::cycle(steps < 0.0);
                    update_keyboard();
                }
                if hit == Some(Kind::Control) {
                    AUDIO.with(|a| {
                        if let Ok(a) = a.try_borrow()
                            && let Some(a) = a.as_ref()
                            && let Some((level, _)) = a.state()
                        {
                            a.set_level(level + 0.02 * steps);
                        }
                    });
                }
                LRESULT(0)
            }
            WM_TIMER => {
                match wp.0 {
                    TIMER_CLOCK => update_clock(),
                    TIMER_STATS => {
                        update_stats();
                        update_battery();
                        panel::notify(crate::control::TICK, 0);
                    }
                    TIMER_USAGE => update_usage(),
                    TIMER_KEYBOARD => {
                        update_keyboard();
                        update_space();
                    }
                    TIMER_SLOW => slow_tick(),
                    TIMER_AUTOHIDE => with_bar(hwnd, |b| b.autohide_tick()).unwrap_or(()),
                    TIMER_REBUILD => {
                        let _ = KillTimer(Some(hwnd), TIMER_REBUILD);
                        rebuild();
                    }
                    // Com a animação de minimizar na tela, espera ela acabar (ler a cor custa quadros).
                    TIMER_TINT | TIMER_TINT_EVENT if crate::minimize::busy() => {}
                    TIMER_TINT => {
                        with_bar(hwnd, |b| b.update_tint());
                    }
                    TIMER_TINT_EVENT => {
                        let _ = KillTimer(Some(hwnd), TIMER_TINT_EVENT);
                        with_bar(hwnd, |b| {
                            b.tint_queued = false;
                            b.update_tint();
                        });
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            status::WM_AUDIO => {
                if wp.0 == 1 {
                    AUDIO.with(|a| {
                        if let Ok(mut a) = a.try_borrow_mut()
                            && let Some(a) = a.as_mut()
                        {
                            a.bind();
                        }
                    });
                }
                panel::notify(msg, wp.0);
                LRESULT(0)
            }
            crate::topextra::WM_EXTRA => {
                for_bars(|b| {
                    b.relayout();
                    b.redraw();
                });
                panel::notify(msg, wp.0);
                LRESULT(0)
            }
            crate::usage::WM_USAGE => {
                for_bars(|b| {
                    b.relayout();
                    b.redraw();
                });
                panel::notify(msg, wp.0);
                LRESULT(0)
            }
            crate::launcher::WM_ICONS => {
                crate::launcher::icons_arrived();
                panel::notify(msg, wp.0);
                LRESULT(0)
            }
            crate::bluetooth::WM_BT_RADIO
            | crate::bluetooth::WM_BT_PANEL
            | crate::wifi::WM_WIFI
            | crate::control::WM_UPDATE => {
                panel::notify(msg, wp.0);
                LRESULT(0)
            }
            WM_APPBAR => {
                with_bar(hwnd, |b| match wp.0 as u32 {
                    ABN_POSCHANGED => b.place(),
                    // O aviso pode vir para todas as AppBars: só esconde a do monitor em tela cheia.
                    ABN_FULLSCREENAPP if lp.0 != 0 => {
                        if MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTONEAREST) == b.hmon {
                            b.set_fullscreen(true);
                        }
                    }
                    ABN_FULLSCREENAPP => b.set_fullscreen(false),
                    _ => {}
                });
                LRESULT(0)
            }
            WM_DISPLAYCHANGE => {
                SetTimer(Some(MAIN.get()), TIMER_REBUILD, REBUILD_MS, None);
                LRESULT(0)
            }
            WM_DPICHANGED => {
                // A posição vem do monitor (rebuild), não do retângulo sugerido.
                let dpi = (wp.0 & 0xffff) as f32;
                if with_bar(hwnd, |b| b.scale != dpi / 96.0) != Some(false) {
                    SetTimer(Some(MAIN.get()), TIMER_REBUILD, REBUILD_MS, None);
                }
                LRESULT(0)
            }
            WM_SETTINGCHANGE => {
                // Pode ser a troca de tema claro/escuro ou de wallpaper.
                with_bar(hwnd, |b| {
                    b.wall_since = None;
                    b.redraw();
                    SetTimer(Some(hwnd), TIMER_TINT, 300, None);
                });
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            // Chegam a todas as barras: só a principal trata.
            WM_TIMECHANGE if main => {
                update_clock();
                LRESULT(0)
            }
            WM_POWERBROADCAST if main => {
                // Volta da suspensão ou mudança na energia: relógio e bateria na hora.
                update_clock();
                panel::notify(crate::control::TICK, 0);
                LRESULT(1)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// Escala (DPI efetivo / 96) do monitor.
fn monitor_scale(hmon: HMONITOR) -> f32 {
    let (mut dx, mut dy) = (96u32, 96u32);
    let _ = unsafe { GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) };
    dx as f32 / 96.0
}

/// Monitores ativos (handle e retângulo inteiro), o principal primeiro.
fn monitors() -> Vec<(HMONITOR, RECT)> {
    unsafe extern "system" fn add(hmon: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
        let list = unsafe { &mut *(lp.0 as *mut Vec<(HMONITOR, RECT)>) };
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if unsafe { GetMonitorInfoW(hmon, &mut mi) }.as_bool() {
            let entry = (hmon, mi.rcMonitor);
            if mi.dwFlags & MONITORINFOF_PRIMARY != 0 {
                list.insert(0, entry);
            } else {
                list.push(entry);
            }
        }
        BOOL(1)
    }
    let mut list = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(add), LPARAM(&mut list as *mut _ as isize));
    }
    list
}

/// Par de janelas de uma barra (vidro atrás, conteúdo em camadas à frente), já como AppBar.
unsafe fn new_windows() -> Option<(HWND, HWND)> {
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).ok()?.into();
        let ex = WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE;
        // Vidro: sem superfície GDI, o conteúdo vem só da composição (glass.rs).
        let glass = CreateWindowExW(
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
        )
        .ok()?;
        // Conteúdo: janela em camadas, "dona" pelo vidro para ficar sempre à frente dele.
        let class = w!("ArcDockWinTopbar");
        let Ok(hwnd) =
            CreateWindowExW(ex | WS_EX_LAYERED, class, class, WS_POPUP, 0, 0, 0, 0, Some(glass), None, Some(hinst), None)
        else {
            let _ = DestroyWindow(glass);
            return None;
        };
        // AppBar na borda de cima: janelas maximizadas ficam abaixo da barra.
        let mut abd = APPBARDATA {
            cbSize: size_of::<APPBARDATA>() as u32,
            hWnd: hwnd,
            uCallbackMessage: WM_APPBAR,
            uEdge: ABE_TOP,
            ..Default::default()
        };
        SHAppBarMessage(ABM_NEW, &mut abd);
        Some((hwnd, glass))
    }
}

/// O Explorer reiniciou e esqueceu as AppBars: sem isto a área de trabalho volta a ocupar a
/// tela toda e janelas maximizadas ficam por baixo da barra (botões de fechar escondidos).
/// Registra cada barra de novo e repõe a reserva da borda de cima.
pub fn explorer_restarted() {
    for_bars(|b| {
        let mut abd = b.appbar_data();
        unsafe {
            SHAppBarMessage(ABM_NEW, &mut abd);
        }
        b.place();
    });
}

/// Tira a AppBar do sistema e destrói as janelas.
fn remove(mut bar: Bar) {
    let mut abd = bar.appbar_data();
    bar.glass_fx = None;
    unsafe {
        SHAppBarMessage(ABM_REMOVE, &mut abd);
        let _ = DestroyWindow(bar.hwnd);
        let _ = DestroyWindow(bar.glass);
    }
}

/// Uma barra por monitor. A do principal é sempre a primeira e mantém a janela (MAIN);
/// barras de monitores que mudaram de área ou escala são refeitas nas mesmas janelas.
fn rebuild() {
    let mut mons = monitors();
    if !config::bar_all_monitors() {
        mons.truncate(1);
    }
    if mons.is_empty() {
        return;
    }
    // Fora de BARS durante a troca: mensagens reentrantes só são ignoradas.
    let Some(mut old) = BARS.with(|b| b.try_borrow_mut().ok().map(|mut g| std::mem::take(&mut *g))) else {
        return;
    };
    let shared = old.first().map(|b| (b.tray.clone(), b.clock.clone(), b.cpu, b.ram));
    if old.len() > mons.len() {
        old.drain(mons.len()..).for_each(remove);
    }
    let mut old = old.into_iter();
    let mut bars = Vec::with_capacity(mons.len());
    for &(hmon, rc) in &mons {
        let mut prev = old.next();
        if prev.as_ref().is_some_and(|b| b.monitor == rc && b.scale == monitor_scale(hmon))
            && let Some(mut b) = prev.take()
        {
            b.hmon = hmon;
            b.place();
            bars.push(b);
            continue;
        }
        // Mesmas janelas (e vidro) da barra anterior nesta posição, ou janelas novas.
        let (hwnd, glass, fx) = match prev {
            Some(mut b) => (b.hwnd, b.glass, b.glass_fx.take()),
            None => match unsafe { new_windows() } {
                Some((h, g)) => (h, g, None),
                None => continue,
            },
        };
        let Ok(mut b) = (unsafe { Bar::new(hwnd, glass, hmon, rc, fx) }) else {
            continue;
        };
        if let Some((tray, clock, cpu, ram)) = &shared {
            (b.tray, b.clock, b.cpu, b.ram) = (tray.clone(), clock.clone(), *cpu, *ram);
        }
        b.relayout();
        b.place();
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            SetTimer(Some(hwnd), TIMER_TINT, TINT_DEBOUNCE_MS, None);
        }
        bars.push(b);
    }
    if let Some(b) = bars.first() {
        MAIN.set(b.hwnd);
    }
    BARS.with(|b| *b.borrow_mut() = bars);
}

/// Opção "todos os monitores" mudou: cria ou tira as barras dos outros monitores.
pub fn monitors_changed() {
    if active() {
        rebuild();
    }
}

/// Cria as topbars (depois do dock: reaproveita a classe da janela de vidro dele).
pub fn create() {
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return };
        let wc = WNDCLASSW {
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: module.into(),
            lpszClassName: w!("ArcDockWinTopbar"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        rebuild();
        let hwnd = MAIN.get();
        if hwnd.is_invalid() {
            return;
        }
        // Avisa mudanças de som (WM_AUDIO); a roda do mouse no botão da central muda o volume.
        AUDIO.with(|a| *a.borrow_mut() = Audio::new(hwnd));
        USAGE.with(|u| *u.borrow_mut() = Some(status::Usage::new()));
        update_clock();
        SetTimer(Some(hwnd), TIMER_STATS, STATS_MS, None);
        SetTimer(Some(hwnd), TIMER_USAGE, crate::usage::REFRESH_MS, None);
        update_usage();
        SetTimer(Some(hwnd), TIMER_KEYBOARD, KEYBOARD_MS, None);
        update_keyboard();
        SetTimer(Some(hwnd), TIMER_SLOW, SLOW_MS, None);
        SLOW_TICKS.set(0);
        update_focus_app();
        update_space();
        slow_tick();
        crate::bluetooth::watch(hwnd);
        // Eventos de janela (foco, minimizar, mover, maximizar) relêem a cor do app.
        let hooks = [
            (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND),
            (EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE),
            (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
        ]
        .into_iter()
        .map(|(from, to)| SetWinEventHook(from, to, None, Some(on_window_event), 0, 0, WINEVENT_OUTOFCONTEXT))
        .filter(|h| !h.is_invalid())
        .collect();
        HOOKS.with(|h| *h.borrow_mut() = hooks);
    }
}

/// Desliga a barra de vez (opção das Configurações): devolve a reserva do topo, destrói
/// janelas e vidros (os timers vão junto), tira os ganchos e solta som e CPU/RAM.
/// `create` a monta de novo.
pub fn destroy() {
    if !active() {
        return;
    }
    panel::close();
    HOOKS.with(|h| {
        for hook in h.take() {
            unsafe {
                let _ = UnhookWinEvent(hook);
            }
        }
    });
    let bars = BARS.with(|b| b.try_borrow_mut().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default());
    bars.into_iter().for_each(remove);
    AUDIO.with(|a| *a.borrow_mut() = None);
    USAGE.with(|u| *u.borrow_mut() = None);
    KEYBOARD_SEEN.set((0, 0));
    KEYBOARD.with_borrow_mut(String::clear);
    MAIN.set(HWND(std::ptr::null_mut()));
    TRAY_BAR.set(HWND(std::ptr::null_mut()));
}

/// Tira as barras do sistema de AppBars (chamado onde o dock encerra).
pub fn shutdown() {
    let bars = BARS.with(|b| b.try_borrow_mut().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default());
    for bar in bars {
        let mut abd = bar.appbar_data();
        unsafe {
            SHAppBarMessage(ABM_REMOVE, &mut abd);
            let _ = ShowWindow(bar.hwnd, SW_HIDE);
            let _ = ShowWindow(bar.glass, SW_HIDE);
        }
    }
}

/// Algo mudou numa janela de topo: troca de app pinta na hora; o resto agenda uma releitura
/// em cada barra.
unsafe extern "system" fn on_window_event(
    _: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    obj: i32,
    child: i32,
    _: u32,
    _: u32,
) {
    if hwnd.is_invalid() || obj != OBJID_WINDOW.0 || child != CHILDID_SELF as i32 {
        return;
    }
    let ours = BARS.with(|b| b.try_borrow().ok().map(|g| g.iter().any(|b| hwnd == b.hwnd || hwnd == b.glass)));
    // A animação de minimizar (minimize.rs) gera um evento por quadro.
    if ours != Some(false) || hwnd == crate::minimize::overlay() {
        return;
    }
    // Só janelas de topo: as internas dos apps (vídeo, progresso...) geram eventos sem parar.
    if unsafe { GetAncestor(hwnd, GA_ROOT) } != hwnd {
        return;
    }
    for_bars(|b| {
        if event == EVENT_SYSTEM_FOREGROUND {
            // Troca de app: pinta já (com a cor do banco não precisa ler a tela).
            b.update_tint();
        }
        // Releitura logo depois, para pegar o fim de animações (restaurar, maximizar).
        if !b.tint_queued {
            b.tint_queued = true;
            unsafe { SetTimer(Some(b.hwnd), TIMER_TINT_EVENT, TINT_THROTTLE_MS, None) };
        }
    });
    // Cada app tem o próprio layout: a sigla acompanha a troca de app na hora.
    if event == EVENT_SYSTEM_FOREGROUND {
        update_keyboard();
        update_focus_app();
    }
}
