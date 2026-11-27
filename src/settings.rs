//! Janela de Configurações do ArcDockWin: janela normal do Windows (barra de título, Alt+Tab,
//! ícone no dock) com fundo Mica, barra lateral de seções e cartões à direita. O desenho usa
//! o mesmo kit dos painéis (`panel.rs`): paleta, texto, interruptor e controle deslizante.
//! As opções ficam em `config.rs` (a animação em `animcfg.rs`) e valem na hora.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};

use tiny_skia::{PixmapMut, Rect, Stroke, Transform};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_TEXT_ALIGNMENT, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, EndPaint, GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromPoint, PAINTSTRUCT, SRCCOPY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_DOWN, VK_LEFT, VK_RETURN,
    VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, HSTRING, w};

use crate::animcfg::{self, Effect};
use crate::config::{self, Module, Slot, Zone as BarZone};
use crate::i18n::tr;
use crate::panel::{self, Buffer, Font, Palette, Text, TextOp, WHITE, paint, rgba};
use crate::topbar::rounded_rect;

const URL: &str = "https://github.com/claudsondouglas/ArcDockWin";
const WM_MOUSELEAVE: u32 = 0x02A3;
/// Área interna da janela e largura da barra lateral (px antes da escala).
const WIDTH: f32 = 820.0;
const HEIGHT: f32 = 620.0;
const SIDEBAR: f32 = 230.0;
const ROW: f32 = 60.0;
/// Linhas mais baixas na página da barra do topo (cabe tudo sem rolagem).
const ROW_COMPACT: f32 = 48.0;
const LOOK_PAGE: usize = 1;
const BAR_PAGE: usize = 3;
const CARD_RADIUS: f32 = 10.0;

/// Seções da barra lateral: nome e glifo (Segoe Fluent Icons).
const PAGES: [(&str, u16); 12] = [
    ("Dock", 0xE71D),
    ("Aparência", 0xE790),
    ("Animações", 0xE768),
    ("Barra do topo", 0xE7F4),
    (crate::extras::PAGES[0].0, crate::extras::PAGES[0].1),
    (crate::extras::PAGES[1].0, crate::extras::PAGES[1].1),
    (crate::extras::PAGES[2].0, crate::extras::PAGES[2].1),
    (crate::extras::PAGES[3].0, crate::extras::PAGES[3].1),
    (crate::extras::PAGES[4].0, crate::extras::PAGES[4].1),
    (crate::extras::PAGES[5].0, crate::extras::PAGES[5].1),
    (crate::extras::PAGES[6].0, crate::extras::PAGES[6].1),
    ("Geral", 0xE713),
];
/// Primeira página das funções extras (`extras.rs`).
const EXTRA_PAGE: usize = 4;

#[derive(Clone, Copy, PartialEq)]
enum Opt {
    IconSize,
    Radius,
    Opacity,
    Padding,
    SidePadding,
    Gap,
    Margin,
    Magnify,
    MagnifySize,
    AutoHide,
    Downloads,
    Trash,
    TaskbarMode,
    TopbarEnabled,
    DockAllMonitors,
    Previews,
    Anim,
    Clock24,
    ClockSeconds,
    ClockDate,
    AllMonitors,
    Autostart,
    WinKey,
    AutoUpdate,
    /// Opção extra (índice em `extras::EXTRAS`).
    Ext(usize),
}

/// O que uma área clicável faz.
#[derive(Clone, Copy, PartialEq)]
enum Act {
    Page(usize),
    Toggle(Opt),
    /// Trilho do controle deslizante: x inicial e final.
    Slider(Opt, f32, f32),
    Effect(Effect),
    Link,
    /// Procurar / instalar atualização, ou abrir a página do release se falhou.
    Update,
    /// Traz de novo os fixados da barra de tarefas do Windows.
    ImportPins,
    /// Módulo da barra do topo: liga/desliga, zona, sobe (-1) ou desce (+1).
    ModOn(Module),
    ModZone(Module, BarZone),
    ModMove(Module, i32),
    /// Escolha de uma opção extra: índice e opção.
    Choice(usize, usize),
}

enum Row {
    Toggle(Opt, &'static str, bool),
    /// Interruptor com uma linha de explicação embaixo do texto.
    ToggleSub(Opt, &'static str, &'static str, bool),
    /// Opção, texto, valor (0..1), rótulo do valor, habilitado.
    Slider(Opt, &'static str, f32, String, bool),
    /// Gênio/Escala; habilitado.
    Effect(bool),
    Info(&'static str, String),
    Link(&'static str, &'static str),
    /// Botão numa linha: texto, texto do botão e o que faz.
    Button(&'static str, &'static str, Act),
    /// Atualizações: texto, valor (já traduzidos) e se dá para clicar.
    Update(String, String, bool),
    /// Módulo da barra; pode subir / descer dentro da zona.
    Module(Slot, bool, bool),
    /// Opção extra (índice em `extras::EXTRAS`).
    Ext(usize),
}

enum Block {
    Label(&'static str),
    Card(Vec<Row>),
    /// Esquema da barra do topo: as três zonas com os módulos visíveis.
    Preview,
}

struct Zone {
    r: [f32; 4],
    radius: f32,
    act: Act,
}

struct Win {
    hwnd: HWND,
    scale: f32,
    text: Text,
    buf: Option<Buffer>,
    page: usize,
    /// Rolagem do conteúdo da seção (px já na escala) e o máximo dela.
    scroll: f32,
    max_scroll: f32,
    zones: Vec<Zone>,
    hot: Option<usize>,
    pressed: Option<usize>,
    /// Arrastando um controle deslizante: opção e trilho.
    drag: Option<(Opt, f32, f32)>,
    /// Área com o foco do teclado (só aparece depois do Tab).
    focus: Option<usize>,
    tracking: bool,
}

thread_local! {
    static WIN: RefCell<Option<Win>> = const { RefCell::new(None) };
}

/// Janela aberta (HWND como número; 0 = fechada), para o dock listá-la como app.
static HWND_OPEN: AtomicIsize = AtomicIsize::new(0);

pub fn hwnd() -> HWND {
    HWND(HWND_OPEN.load(Ordering::Relaxed) as *mut c_void)
}

fn with_win<R>(f: impl FnOnce(&mut Win) -> R) -> Option<R> {
    // try_borrow_mut: chamadas do Windows podem reentrar no wndproc.
    WIN.with(|w| w.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

/// Controles deslizantes em px (ou %) inteiros: mínimo, máximo, leitura e gravação.
fn px_slider(o: Opt) -> Option<(f32, f32, fn() -> f32, fn(f32))> {
    Some(match o {
        Opt::IconSize => (32.0, 72.0, config::icon_size as fn() -> f32, config::set_icon_size as fn(f32)),
        Opt::Radius => (0.0, 40.0, config::dock_radius, config::set_dock_radius),
        Opt::Opacity => (0.0, 100.0, config::dock_opacity, config::set_dock_opacity),
        Opt::Padding => (2.0, 24.0, config::dock_padding, config::set_dock_padding),
        Opt::SidePadding => (0.0, 32.0, config::dock_side_padding, config::set_dock_side_padding),
        Opt::Gap => (0.0, 24.0, config::icon_gap, config::set_icon_gap),
        Opt::Margin => (0.0, 32.0, config::dock_margin, config::set_dock_margin),
        _ => return None,
    })
}

/// Valor (0..1) de um controle deslizante.
fn value(o: Opt) -> f32 {
    if let Some((min, max, get, _)) = px_slider(o) {
        return (get() - min) / (max - min);
    }
    match o {
        Opt::MagnifySize => (config::magnify_scale() - 1.1) / 0.9,
        Opt::Ext(i) => crate::extras::fraction(i),
        _ => 0.0,
    }
}

/// Passo do teclado em cada controle deslizante (1 px ou 1%; 0,05×).
fn step(o: Opt) -> f32 {
    if let Opt::Ext(i) = o {
        return crate::extras::step(i);
    }
    match px_slider(o) {
        Some((min, max, ..)) => 1.0 / (max - min),
        None => 0.05 / 0.9,
    }
}

/// Texto do valor ao lado do controle deslizante.
fn slider_label(o: Opt) -> String {
    match o {
        Opt::Opacity => format!("{}%", config::dock_opacity()),
        Opt::Ext(i) => crate::extras::label(i),
        _ => px_slider(o).map(|(_, _, get, _)| format!("{} px", get())).unwrap_or_default(),
    }
}

/// Muda um controle deslizante e aplica no dock; `reload`: recarrega os ícones (fim do arrasto).
fn slide(o: Opt, v: f32, reload: bool) {
    let v = v.clamp(0.0, 1.0);
    let changed = match o {
        o if px_slider(o).is_some() => {
            let (min, max, get, set) = px_slider(o).unwrap();
            let px = (min + v * (max - min)).round();
            let changed = px != get();
            if changed {
                set(px);
            }
            changed
        }
        Opt::Ext(i) => {
            // As extras se aplicam sozinhas (extras::changed).
            crate::extras::slide(i, v);
            false
        }
        Opt::MagnifySize => {
            let x = ((1.1 + v * 0.9) * 20.0).round() / 20.0;
            let changed = (x - config::magnify_scale()).abs() > 0.001;
            if changed {
                config::set_magnify_scale(x);
            }
            changed
        }
        _ => false,
    };
    if changed || reload {
        crate::apply_settings(reload);
    }
}

fn toggle(o: Opt) {
    match o {
        Opt::Magnify => {
            config::set_magnify(!config::magnify());
            crate::apply_settings(true);
        }
        Opt::AutoHide => {
            config::set_autohide(!config::autohide());
            crate::apply_settings(false);
        }
        Opt::Downloads => {
            config::set_show_downloads(!config::show_downloads());
            crate::apply_settings(false);
        }
        Opt::Trash => {
            config::set_show_trash(!config::show_trash());
            crate::apply_settings(false);
        }
        Opt::TaskbarMode => crate::set_taskbar_mode(!config::taskbar_mode()),
        Opt::TopbarEnabled => crate::set_topbar_enabled(!config::topbar_enabled()),
        Opt::DockAllMonitors => {
            config::set_dock_all_monitors(!config::dock_all_monitors());
            crate::rebuild_docks();
        }
        Opt::Previews => {
            config::set_window_previews(!config::window_previews());
            crate::preview::dismiss();
        }
        Opt::Anim => {
            animcfg::set_enabled(!animcfg::enabled());
            crate::minimize::config_changed();
        }
        Opt::Clock24 => {
            config::set_clock_24h(!config::clock_24h());
            crate::topbar::settings_changed();
        }
        Opt::ClockSeconds => {
            config::set_clock_seconds(!config::clock_seconds());
            crate::topbar::settings_changed();
        }
        Opt::ClockDate => {
            config::set_clock_date(!config::clock_date());
            crate::topbar::settings_changed();
        }
        Opt::AllMonitors => {
            config::set_bar_all_monitors(!config::bar_all_monitors());
            crate::topbar::monitors_changed();
        }
        Opt::Autostart => {
            config::set_autostart(!config::autostart());
            crate::autostart();
        }
        Opt::WinKey => {
            config::set_win_key_launcher(!config::win_key_launcher());
            crate::winkey::config_changed();
        }
        Opt::AutoUpdate => {
            config::set_auto_update(!config::auto_update());
            if config::auto_update() {
                crate::update::check_now();
            }
        }
        Opt::Ext(i) => crate::extras::toggle(i),
        Opt::IconSize | Opt::MagnifySize | Opt::Radius | Opt::Opacity | Opt::Padding | Opt::SidePadding | Opt::Gap | Opt::Margin => {}
    }
}

/// Executa uma ação fora do empréstimo da janela (as ações mexem no dock e na barra).
fn run(act: Act) {
    match act {
        Act::Page(i) => {
            with_win(|w| {
                if w.page != i {
                    w.scroll = 0.0;
                }
                w.page = i;
            });
        }
        Act::Toggle(o) => toggle(o),
        Act::Effect(e) => {
            animcfg::set_effect(e);
            crate::minimize::config_changed();
        }
        Act::Link => unsafe {
            ShellExecuteW(None, w!("open"), &HSTRING::from(URL), None, None, SW_SHOWNORMAL);
        },
        Act::Update => match crate::update::status() {
            crate::update::Status::Available(_) => crate::update::install(),
            crate::update::Status::Failed => crate::update::open_release_page(),
            _ => crate::update::check_now(),
        },
        Act::ModOn(m) => {
            config::set_module_on(m, !config::module_on(m));
            crate::topbar::settings_changed();
        }
        Act::ModZone(m, z) => {
            config::set_module_zone(m, z);
            crate::topbar::settings_changed();
        }
        Act::ModMove(m, dir) => {
            config::move_module(m, dir);
            crate::topbar::settings_changed();
        }
        Act::ImportPins => {
            crate::apps::import_taskbar_pins();
            crate::reload_pins();
        }
        Act::Choice(i, c) => crate::extras::set_choice(i, c),
        Act::Slider(..) => {}
    }
}

/// Número com vírgula em pt-BR.
fn decimal(v: f32) -> String {
    let s = format!("{v:.2}");
    if crate::i18n::is_traditional_chinese() || crate::i18n::is_english() { s } else { s.replace('.', ",") }
}

/// Linha de atualizações do "Sobre", conforme o estado da busca.
fn update_row() -> Row {
    use crate::update::Status;
    let (label, value, active) = match crate::update::status() {
        Status::Idle => ("Atualizações", tr("Procurar agora"), true),
        Status::Checking => ("Atualizações", tr("Procurando…"), false),
        Status::UpToDate => ("Você tem a versão mais recente", tr("Procurar agora"), true),
        Status::Available(v) => ("Nova versão disponível", format!("{} v{v}", tr("Instalar")), true),
        Status::Downloading => ("Atualizando…", String::new(), false),
        Status::Failed => ("Não foi possível atualizar", tr("Baixar manualmente"), true),
    };
    Row::Update(tr(label), value, active)
}

/// Estado da atualização mudou: redesenha, se a janela estiver aberta.
pub fn refresh() {
    with_win(|w| w.redraw());
}

/// Conteúdo da seção `page`.
fn blocks(page: usize) -> Vec<Block> {
    match page {
        0 => {
            let zoom = config::magnify();
            vec![Block::Card(vec![
                Row::Slider(Opt::IconSize, "Tamanho dos ícones", value(Opt::IconSize), slider_label(Opt::IconSize), true),
                Row::Toggle(Opt::Magnify, "Ampliação ao passar o mouse", zoom),
                Row::Slider(
                    Opt::MagnifySize,
                    "Tamanho da ampliação",
                    value(Opt::MagnifySize),
                    format!("{}×", decimal(config::magnify_scale())),
                    zoom,
                ),
                Row::Toggle(Opt::AutoHide, "Ocultar automaticamente", config::autohide()),
                Row::Toggle(Opt::Downloads, "Mostrar Downloads", config::show_downloads()),
                Row::Toggle(Opt::Trash, "Mostrar Lixeira", config::show_trash()),
                Row::Toggle(Opt::Previews, "Prévia das janelas ao passar o mouse", config::window_previews()),
                Row::Toggle(Opt::DockAllMonitors, "Mostrar Dock em todos os monitores", config::dock_all_monitors()),
            ])]
        }
        LOOK_PAGE => {
            let slider = |o: Opt, t: &'static str| Row::Slider(o, t, value(o), slider_label(o), true);
            vec![
                Block::Card(vec![
                    slider(Opt::Radius, "Arredondamento dos cantos"),
                    slider(Opt::Opacity, "Opacidade do fundo"),
                    slider(Opt::Padding, "Espaço entre os ícones e a borda"),
                    slider(Opt::SidePadding, "Espaço extra nas laterais"),
                    slider(Opt::Gap, "Espaço entre os ícones"),
                    slider(Opt::Margin, "Distância da borda da tela"),
                ]),
                Block::Card(vec![Row::ToggleSub(
                    Opt::TaskbarMode,
                    "Modo barra de tarefas",
                    "Dock sempre à vista; janelas maximizadas param acima dele",
                    config::taskbar_mode(),
                )]),
            ]
        }
        2 => {
            let on = animcfg::enabled();
            vec![Block::Card(vec![Row::Toggle(Opt::Anim, "Animação ao minimizar janelas", on), Row::Effect(on)])]
        }
        BAR_PAGE => {
            // Desligada, a barra não existe: só o interruptor (e o aviso sobre a bandeja).
            if !config::topbar_enabled() {
                return vec![Block::Card(vec![Row::ToggleSub(
                    Opt::TopbarEnabled,
                    "Mostrar barra superior",
                    "Sem ela, os ícones da bandeja do sistema ficam ocultos",
                    false,
                )])];
            }
            // Ordem da lista = ordem na barra, da esquerda para a direita.
            let list = config::modules();
            let rows = list
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let up = i > 0 && list[i - 1].zone == s.zone;
                    let down = list.get(i + 1).is_some_and(|n| n.zone == s.zone);
                    Row::Module(*s, up, down)
                })
                .collect();
            vec![
                Block::Card(vec![
                    Row::ToggleSub(Opt::TopbarEnabled, "Mostrar barra superior", "Desligar economiza memória e CPU", true),
                    Row::Toggle(Opt::AllMonitors, "Mostrar em todos os monitores", config::bar_all_monitors()),
                ]),
                Block::Preview,
                Block::Card(rows),
                Block::Card(vec![
                    Row::Toggle(Opt::Clock24, "Relógio em 24 horas", config::clock_24h()),
                    Row::Toggle(Opt::ClockSeconds, "Mostrar segundos", config::clock_seconds()),
                    Row::Toggle(Opt::ClockDate, "Mostrar a data", config::clock_date()),
                ]),
            ]
        }
        p if (EXTRA_PAGE..EXTRA_PAGE + crate::extras::PAGES.len()).contains(&p) => crate::extras::PAGES[p - EXTRA_PAGE]
            .2
            .iter()
            .map(|card| Block::Card(card.iter().filter_map(|k| crate::extras::index(k)).map(Row::Ext).collect()))
            .collect(),
        _ => vec![
            Block::Card(vec![
                Row::Toggle(Opt::Autostart, "Iniciar com o Windows", config::autostart()),
                Row::Toggle(Opt::WinKey, "Tecla Windows abre o lançador", config::win_key_launcher()),
                Row::Toggle(Opt::AutoUpdate, "Procurar atualizações automaticamente", config::auto_update()),
                Row::Button("Fixados da barra de tarefas do Windows", "Importar", Act::ImportPins),
            ]),
            Block::Label("Sobre"),
            Block::Card(vec![
                Row::Info("ArcDockWin", format!("{} {}", tr("Versão"), env!("CARGO_PKG_VERSION"))),
                update_row(),
                Row::Link("Repositório", "github.com/claudsondouglas/ArcDockWin"),
            ]),
        ],
    }
}

impl Win {
    fn redraw(&mut self) {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        let (bw, bh) = (rc.right, rc.bottom);
        if bw <= 0 || bh <= 0 {
            return;
        }
        if self.buf.as_ref().is_none_or(|b| b.w != bw || b.h != bh) {
            self.buf = None;
            self.buf = Buffer::new(bw, bh);
        }
        let Some(buf) = &self.buf else { return };
        let (memdc, bits) = (buf.memdc, buf.bits);
        let s = self.scale;
        let pal = Palette::new();
        // Fundo transparente: o Mica aparece por trás (moldura estendida sobre a janela toda).
        let bytes = unsafe { std::slice::from_raw_parts_mut(bits, (bw * bh * 4) as usize) };
        bytes.fill(0);
        let Some(mut pm) = PixmapMut::from_bytes(bytes, bw as u32, bh as u32) else { return };
        let tf = Transform::identity();
        let fill = |pm: &mut PixmapMut, [x, y, w, h]: [f32; 4], r: f32, c: [u8; 4]| {
            if let Some(p) = rounded_rect(x, y, w, h, r) {
                pm.fill_path(&p, &paint(c), tiny_skia::FillRule::Winding, tf, None);
            }
        };
        let stroke = |pm: &mut PixmapMut, [x, y, w, h]: [f32; 4], r: f32, c: [u8; 4], width: f32| {
            if let Some(p) = rounded_rect(x, y, w, h, r) {
                pm.stroke_path(&p, &paint(c), &Stroke { width, ..Default::default() }, tf, None);
            }
        };
        let texts: RefCell<Vec<TextOp>> = RefCell::new(Vec::new());
        let mut text = |t: &str, font: Font, rect: [f32; 4], align: DWRITE_TEXT_ALIGNMENT, color: [f32; 4]| {
            texts.borrow_mut().push(TextOp { text: t.encode_utf16().collect(), font, rect, align, color, wrap: false, clip: None });
        };
        let glyph = |g: u16| String::from_utf16_lossy(&[g]);
        let (lead, trail, center) = (DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_ALIGNMENT_CENTER);
        let mut zones = Vec::new();

        // Barra lateral: seção atual com fundo realçado e um traço na cor de destaque.
        let side = SIDEBAR * s;
        for (i, &(name, g)) in PAGES.iter().enumerate() {
            let r = [12.0 * s, (12.0 + i as f32 * 40.0) * s, side - 24.0 * s, 36.0 * s];
            if i == self.page {
                fill(&mut pm, r, 6.0 * s, pal.hover);
                fill(&mut pm, [r[0], r[1] + 10.0 * s, 3.0 * s, r[3] - 20.0 * s], 1.5 * s, pal.accent);
            }
            zones.push(Zone { r, radius: 6.0 * s, act: Act::Page(i) });
            text(&glyph(g), Font::Icon, [r[0] + 12.0 * s, r[1], 20.0 * s, r[3]], center, pal.ink);
            text(&tr(name), Font::Body, [r[0] + 44.0 * s, r[1], r[2] - 52.0 * s, r[3]], lead, pal.ink);
        }

        // Conteúdo: título da seção e cartões.
        let (cx, cr) = (side + 8.0 * s, bw as f32 - 28.0 * s);
        let cw = cr - cx;
        let side_texts = texts.borrow().len();
        let side_zones = zones.len();
        let mut y = 72.0 * s - self.scroll;
        for block in blocks(self.page) {
            let rows = match block {
                Block::Label(t) => {
                    text(&tr(t), Font::Bold, [cx + 4.0 * s, y + 8.0 * s, cw, 24.0 * s], lead, pal.ink);
                    y += 36.0 * s;
                    continue;
                }
                Block::Preview => {
                    preview(&mut pm, &mut text, &pal, [cx, y, cw, 44.0 * s], s);
                    y += 44.0 * s + 16.0 * s;
                    continue;
                }
                Block::Card(rows) => rows,
            };
            let compact = self.page == BAR_PAGE || (EXTRA_PAGE..EXTRA_PAGE + crate::extras::PAGES.len()).contains(&self.page);
            let rh = if compact { ROW_COMPACT } else { ROW } * s;
            let card = [cx, y, cw, rows.len() as f32 * rh];
            fill(&mut pm, card, CARD_RADIUS * s, pal.card);
            stroke(&mut pm, [cx + 0.5, y + 0.5, cw - 1.0, card[3] - 1.0], CARD_RADIUS * s - 0.5, pal.card_line, 1.0);
            for (k, row) in rows.iter().enumerate() {
                let ry = y + k as f32 * rh;
                if k > 0
                    && let Some(line) = Rect::from_xywh(cx + 16.0 * s, ry, cw - 32.0 * s, 1.0)
                {
                    pm.fill_rect(line, &paint(pal.line), tf, None);
                }
                let (lx, rx, cy) = (cx + 18.0 * s, cr - 18.0 * s, ry + rh / 2.0);
                let label = [lx, ry, rx - lx, rh];
                let whole = [cx + 4.0 * s, ry + 4.0 * s, cw - 8.0 * s, rh - 8.0 * s];
                match row {
                    Row::Toggle(o, t, on) => {
                        let sx = panel::switch(&mut pm, tf, &pal, rx, cy, s, *on);
                        zones.push(Zone { r: whole, radius: 8.0 * s, act: Act::Toggle(*o) });
                        text(&tr(t), Font::Body, [lx, ry, sx - lx - 8.0 * s, rh], lead, pal.ink);
                    }
                    Row::ToggleSub(o, t, sub, on) => {
                        let sx = panel::switch(&mut pm, tf, &pal, rx, cy, s, *on);
                        zones.push(Zone { r: whole, radius: 8.0 * s, act: Act::Toggle(*o) });
                        let w = sx - lx - 8.0 * s;
                        text(&tr(t), Font::Body, [lx, cy - 19.0 * s, w, 20.0 * s], lead, pal.ink);
                        text(&tr(sub), Font::Small, [lx, cy + 1.0 * s, w, 18.0 * s], lead, pal.dim);
                    }
                    Row::Slider(o, t, v, value, on) => {
                        let vw = 64.0 * s;
                        let (tl, tr_) = (rx - vw - 16.0 * s - 220.0 * s, rx - vw - 16.0 * s);
                        let color = if *on { pal.accent } else { pal.strong };
                        panel::slider(&mut pm, tf, &pal, tl, tr_, cy, s, v.clamp(0.0, 1.0), color);
                        if *on {
                            let r = [tl - 10.0 * s, ry, tr_ - tl + 20.0 * s, rh];
                            zones.push(Zone { r, radius: 0.0, act: Act::Slider(*o, tl, tr_) });
                        }
                        let ink = if *on { pal.ink } else { pal.dim };
                        text(&tr(t), Font::Body, [lx, ry, tl - lx - 16.0 * s, rh], lead, ink);
                        text(value, Font::Body, [rx - vw, ry, vw, rh], trail, ink);
                    }
                    Row::Effect(on) => {
                        let (sw, sh) = (100.0 * s, 30.0 * s);
                        let x0 = rx - 2.0 * sw;
                        fill(&mut pm, [x0, cy - sh / 2.0, 2.0 * sw, sh], 8.0 * s, pal.control);
                        for (i, e) in Effect::ALL.into_iter().enumerate() {
                            let r = [x0 + i as f32 * sw, cy - sh / 2.0, sw, sh];
                            let selected = e == animcfg::effect();
                            if selected {
                                let c = if *on { pal.accent } else { pal.strong };
                                fill(&mut pm, [r[0] + 3.0 * s, r[1] + 3.0 * s, sw - 6.0 * s, sh - 6.0 * s], 6.0 * s, c);
                            } else if *on {
                                zones.push(Zone { r, radius: 8.0 * s, act: Act::Effect(e) });
                            }
                            let ink = match (*on, selected) {
                                (true, true) => WHITE,
                                (true, false) => pal.ink,
                                _ => pal.dim,
                            };
                            text(&tr(e.label()), Font::Body, r, center, ink);
                        }
                        text(&tr("Efeito"), Font::Body, [lx, ry, x0 - lx - 8.0 * s, rh], lead, if *on { pal.ink } else { pal.dim });
                    }
                    Row::Module(slot, up, down) => {
                        let m = slot.module;
                        let sx = panel::switch(&mut pm, tf, &pal, rx, cy, s, slot.on);
                        zones.push(Zone { r: [sx - 4.0 * s, cy - 14.0 * s, rx - sx + 8.0 * s, 28.0 * s], radius: 14.0 * s, act: Act::ModOn(m) });
                        // Setas ▲ ▼: trocam com o vizinho da mesma zona.
                        let bw = 28.0 * s;
                        let mut bx = sx - 14.0 * s - 2.0 * bw - 4.0 * s;
                        for (dir, g, enabled) in [(-1, 0xE70E, *up), (1, 0xE70D, *down)] {
                            let r = [bx, cy - bw / 2.0, bw, bw];
                            fill(&mut pm, r, 6.0 * s, pal.control);
                            if enabled {
                                zones.push(Zone { r, radius: 6.0 * s, act: Act::ModMove(m, dir) });
                            }
                            let ink = if enabled { pal.ink } else { [pal.dim[0], pal.dim[1], pal.dim[2], pal.dim[3] * 0.5] };
                            text(&glyph(g), Font::Icon, r, center, ink);
                            bx += bw + 4.0 * s;
                        }
                        // Zona: Esquerda | Centro | Direita.
                        let (sw, sh) = (78.0 * s, 28.0 * s);
                        let x0 = sx - 14.0 * s - 2.0 * bw - 4.0 * s - 12.0 * s - 3.0 * sw;
                        fill(&mut pm, [x0, cy - sh / 2.0, 3.0 * sw, sh], 8.0 * s, pal.control);
                        for (i, z) in BarZone::ALL.into_iter().enumerate() {
                            let r = [x0 + i as f32 * sw, cy - sh / 2.0, sw, sh];
                            let selected = z == slot.zone;
                            if selected {
                                let c = if slot.on { pal.accent } else { pal.strong };
                                fill(&mut pm, [r[0] + 3.0 * s, r[1] + 3.0 * s, sw - 6.0 * s, sh - 6.0 * s], 6.0 * s, c);
                            } else {
                                zones.push(Zone { r, radius: 8.0 * s, act: Act::ModZone(m, z) });
                            }
                            text(&tr(z.label()), Font::Body, r, center, if selected { WHITE } else { pal.ink });
                        }
                        let ink = if slot.on { pal.ink } else { pal.dim };
                        text(&tr(m.label()), Font::Body, [lx, ry, x0 - lx - 8.0 * s, rh], lead, ink);
                    }
                    Row::Ext(i) => {
                        use crate::extras::{EXTRAS, Kind};
                        let (i, e) = (*i, &EXTRAS[*i]);
                        let on = crate::extras::enabled(i);
                        let ink = if on { pal.ink } else { pal.dim };
                        match e.kind {
                            Kind::Toggle => {
                                let sx = panel::switch(&mut pm, tf, &pal, rx, cy, s, crate::extras::on(e.key));
                                zones.push(Zone { r: whole, radius: 8.0 * s, act: Act::Toggle(Opt::Ext(i)) });
                                let w = sx - lx - 8.0 * s;
                                if e.sub.is_empty() {
                                    text(&tr(e.label), Font::Body, [lx, ry, w, rh], lead, ink);
                                } else {
                                    text(&tr(e.label), Font::Body, [lx, cy - 18.0 * s, w, 19.0 * s], lead, ink);
                                    text(&tr(e.sub), Font::Small, [lx, cy + 1.0 * s, w, 17.0 * s], lead, pal.dim);
                                }
                            }
                            Kind::Slider { .. } => {
                                let vw = 64.0 * s;
                                let (tl, tr_) = (rx - vw - 16.0 * s - 220.0 * s, rx - vw - 16.0 * s);
                                let color = if on { pal.accent } else { pal.strong };
                                panel::slider(&mut pm, tf, &pal, tl, tr_, cy, s, crate::extras::fraction(i).clamp(0.0, 1.0), color);
                                if on {
                                    let r = [tl - 10.0 * s, ry, tr_ - tl + 20.0 * s, rh];
                                    zones.push(Zone { r, radius: 0.0, act: Act::Slider(Opt::Ext(i), tl, tr_) });
                                }
                                text(&tr(e.label), Font::Body, [lx + 14.0 * s, ry, tl - lx - 30.0 * s, rh], lead, ink);
                                text(&crate::extras::label(i), Font::Body, [rx - vw, ry, vw, rh], trail, ink);
                            }
                            Kind::Choice(opts) if e.key == "bar_color" => {
                                // Cores em bolinhas; a escolhida ganha um anel.
                                let (sw, d) = (30.0 * s, 20.0 * s);
                                let x0 = rx - opts.len() as f32 * sw;
                                let cur = crate::extras::choice(e.key);
                                for k in 0..opts.len() {
                                    let [r, g, b] = crate::extras::bar_color_rgb(k);
                                    let cx_ = x0 + k as f32 * sw + sw / 2.0;
                                    let r_ = [cx_ - d / 2.0, cy - d / 2.0, d, d];
                                    let a = if on { 255 } else { 90 };
                                    fill(&mut pm, r_, d / 2.0, [r, g, b, a]);
                                    stroke(&mut pm, r_, d / 2.0, pal.card_line, 1.0);
                                    if k == cur {
                                        let ring = [r_[0] - 3.0 * s, r_[1] - 3.0 * s, d + 6.0 * s, d + 6.0 * s];
                                        stroke(&mut pm, ring, d / 2.0 + 3.0 * s, pal.accent, 2.0 * s);
                                    } else if on {
                                        zones.push(Zone { r: [x0 + k as f32 * sw, ry, sw, rh], radius: 8.0 * s, act: Act::Choice(i, k) });
                                    }
                                }
                                text(&tr(e.label), Font::Body, [lx + 14.0 * s, ry, x0 - lx - 22.0 * s, rh], lead, ink);
                            }
                            Kind::Choice(opts) => {
                                let wide = opts.iter().any(|o| tr(o).chars().count() > 10);
                                let (sw, sh) = (if wide { 116.0 } else { 84.0 } * s, 28.0 * s);
                                let x0 = rx - opts.len() as f32 * sw;
                                fill(&mut pm, [x0, cy - sh / 2.0, opts.len() as f32 * sw, sh], 8.0 * s, pal.control);
                                let cur = crate::extras::choice(e.key);
                                for (k, o) in opts.iter().enumerate() {
                                    let r = [x0 + k as f32 * sw, cy - sh / 2.0, sw, sh];
                                    if k == cur {
                                        let c = if on { pal.accent } else { pal.strong };
                                        fill(&mut pm, [r[0] + 3.0 * s, r[1] + 3.0 * s, sw - 6.0 * s, sh - 6.0 * s], 6.0 * s, c);
                                    } else if on {
                                        zones.push(Zone { r, radius: 8.0 * s, act: Act::Choice(i, k) });
                                    }
                                    let c = if k == cur && on { WHITE } else { ink };
                                    text(&tr(o), Font::Body, r, center, c);
                                }
                                let x = if e.needs.is_empty() { lx } else { lx + 14.0 * s };
                                text(&tr(e.label), Font::Body, [x, ry, x0 - x - 8.0 * s, rh], lead, ink);
                            }
                        }
                    }
                    Row::Button(t, b, act) => {
                        zones.push(Zone { r: whole, radius: 8.0 * s, act: *act });
                        text(&tr(t), Font::Body, label, lead, pal.ink);
                        text(&tr(b), Font::Body, [lx, ry, rx - lx, rh], trail, rgba(pal.accent));
                    }
                    Row::Info(t, v) => {
                        text(t, Font::Bold, label, lead, pal.ink);
                        text(v, Font::Body, label, trail, pal.dim);
                    }
                    Row::Update(t, v, active) => {
                        let ink = if *active { rgba(pal.accent) } else { pal.dim };
                        if *active {
                            zones.push(Zone { r: whole, radius: 8.0 * s, act: Act::Update });
                        }
                        text(t, Font::Body, label, lead, pal.ink);
                        text(v, Font::Body, [lx, ry, rx - lx, rh], trail, ink);
                    }
                    Row::Link(t, v) => {
                        zones.push(Zone { r: whole, radius: 8.0 * s, act: Act::Link });
                        text(&tr(t), Font::Body, label, lead, pal.ink);
                        text(&glyph(0xE8A7), Font::Icon, [rx - 18.0 * s, ry, 18.0 * s, rh], center, rgba(pal.accent));
                        text(v, Font::Body, [lx, ry, rx - lx - 26.0 * s, rh], trail, rgba(pal.accent));
                    }
                }
            }
            y += card[3] + 16.0 * s;
        }
        // Rolagem: o que passou para baixo do título some (a área do título fica limpa).
        self.max_scroll = (y + self.scroll - bh as f32 + 8.0 * s).max(0.0);
        let head = 64.0 * s;
        if self.scroll > 0.0 {
            let x0 = (side as usize).min(bw as usize);
            let data = pm.data_mut();
            for row in 0..(head as usize).min(bh as usize) {
                let start = (row * bw as usize + x0) * 4;
                let end = (row + 1) * bw as usize * 4;
                data[start..end].fill(0);
            }
            let mut k = 0;
            texts.borrow_mut().retain(|t| {
                k += 1;
                k <= side_texts || t.rect[1] + t.rect[3] / 2.0 >= head
            });
            let mut k = 0;
            zones.retain(|z| {
                k += 1;
                k <= side_zones || z.r[1] + z.r[3] / 2.0 >= head
            });
        }
        text(&tr(PAGES[self.page].0), Font::Header, [cx + 4.0 * s, 10.0 * s, cw, 48.0 * s], lead, pal.ink);

        // Realce do mouse (menos na seção atual e nos controles deslizantes) e anel do foco.
        if let Some(z) = self.hot.and_then(|i| zones.get(i))
            && !matches!(z.act, Act::Slider(..))
            && z.act != Act::Page(self.page)
        {
            fill(&mut pm, z.r, z.radius, pal.hover);
        }
        if let Some(z) = self.focus.and_then(|i| zones.get(i)) {
            let [x, y, w, h] = z.r;
            let ink = pal.ink.map(|v| (v * 255.0) as u8);
            stroke(&mut pm, [x - 1.0 * s, y - 1.0 * s, w + 2.0 * s, h + 2.0 * s], z.radius + 1.0 * s, ink, 2.0 * s);
        }
        self.zones = zones;
        drop(pm);
        let texts = texts.into_inner();
        self.text.draw(memdc, bw, bh, &texts);
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn hit(&self, lp: LPARAM) -> (f32, Option<usize>) {
        let (x, y) = ((lp.0 & 0xffff) as i16 as f32, ((lp.0 >> 16) & 0xffff) as i16 as f32);
        let i = self.zones.iter().position(|z| x >= z.r[0] && x < z.r[0] + z.r[2] && y >= z.r[1] && y < z.r[1] + z.r[3]);
        (x, i)
    }

    /// Valor (0..1) do controle deslizante arrastado com o mouse em `x`.
    fn drag_value(&self, x: f32) -> Option<(Opt, f32)> {
        let (o, l, r) = self.drag?;
        Some((o, (x - l) / (r - l).max(1.0)))
    }

    /// Teclado: Tab/Shift+Tab move o foco, Espaço/Enter aciona, setas mudam valor ou seção.
    fn key(&mut self, vk: u16) -> Option<(Act, f32)> {
        let n = self.zones.len();
        if n == 0 {
            return None;
        }
        if vk == VK_TAB.0 {
            let back = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
            self.focus = Some(match self.focus {
                None if back => n - 1,
                None => 0,
                Some(i) if back => (i + n - 1) % n,
                Some(i) => (i + 1) % n,
            });
            self.redraw();
            return None;
        }
        let i = self.focus.filter(|&i| i < n)?;
        let act = self.zones[i].act;
        let dir = match vk {
            v if v == VK_LEFT.0 || v == VK_UP.0 => -1.0,
            v if v == VK_RIGHT.0 || v == VK_DOWN.0 => 1.0,
            v if v == VK_SPACE.0 || v == VK_RETURN.0 => return Some((act, 0.0)),
            _ => return None,
        };
        match act {
            Act::Slider(o, ..) => Some((act, value(o) + dir * step(o))),
            Act::Page(p) => {
                let p = (p as i32 + dir as i32).clamp(0, PAGES.len() as i32 - 1) as usize;
                self.focus = Some(p);
                Some((Act::Page(p), 0.0))
            }
            _ => None,
        }
    }
}

/// Esquema da barra do topo em `r`: três zonas com os módulos visíveis, na ordem.
fn preview(
    pm: &mut PixmapMut,
    text: &mut impl FnMut(&str, Font, [f32; 4], DWRITE_TEXT_ALIGNMENT, [f32; 4]),
    pal: &Palette,
    [x, y, w, h]: [f32; 4],
    s: f32,
) {
    let tf = Transform::identity();
    let fill = |pm: &mut PixmapMut, [x, y, w, h]: [f32; 4], r: f32, c: [u8; 4]| {
        if let Some(p) = rounded_rect(x, y, w, h, r) {
            pm.fill_path(&p, &paint(c), tiny_skia::FillRule::Winding, tf, None);
        }
    };
    fill(pm, [x, y, w, h], CARD_RADIUS * s, pal.card);
    let zw = w / 3.0;
    let list = config::modules();
    for (zi, zone) in BarZone::ALL.into_iter().enumerate() {
        let zx = x + zi as f32 * zw;
        if zi > 0
            && let Some(line) = Rect::from_xywh(zx, y + 10.0 * s, 1.0, h - 20.0 * s)
        {
            pm.fill_rect(line, &paint(pal.line), tf, None);
        }
        let mods: Vec<&Slot> = list.iter().filter(|m| m.zone == zone && m.on).collect();
        if mods.is_empty() {
            text(&tr(zone.label()), Font::Small, [zx, y, zw, h], DWRITE_TEXT_ALIGNMENT_CENTER, pal.dim);
            continue;
        }
        // Fichas do mesmo tamanho, encostadas no lado da zona (centro: centralizadas).
        let (pad, gap) = (8.0 * s, 4.0 * s);
        let n = mods.len() as f32;
        let cw = ((zw - 2.0 * pad - gap * (n - 1.0)) / n).min(84.0 * s);
        let total = cw * n + gap * (n - 1.0);
        let mut cx = match zone {
            BarZone::Left => zx + pad,
            BarZone::Center => zx + (zw - total) / 2.0,
            BarZone::Right => zx + zw - pad - total,
        };
        for m in mods {
            let r = [cx, y + 9.0 * s, cw, h - 18.0 * s];
            fill(pm, r, 6.0 * s, pal.control);
            text(&tr(m.module.short()), Font::Small, r, DWRITE_TEXT_ALIGNMENT_CENTER, pal.ink);
            cx += cw + gap;
        }
    }
}

/// Tema escuro na barra de título e fundo Mica (Windows 11), conforme o tema do sistema.
fn apply_theme(hwnd: HWND) {
    unsafe {
        let dark = BOOL::from(!crate::light_theme());
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const c_void, 4);
        let mica = DWMSBT_MAINWINDOW;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &mica as *const _ as *const c_void, 4);
        let round = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as *const c_void, 4);
        let all = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &all);
    }
}

/// Abre a janela de Configurações; se já está aberta, traz para a frente.
pub fn open() {
    unsafe {
        let open = hwnd();
        if !open.is_invalid() {
            if IsIconic(open).as_bool() {
                let _ = ShowWindow(open, SW_RESTORE);
            }
            let _ = SetForegroundWindow(open);
            return;
        }
        let Ok(module) = GetModuleHandleW(None) else { return };
        let hinst: HINSTANCE = module.into();
        let class = w!("ArcDockWinSettings");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            // Ícone do app (recurso 1, embutido pelo build.rs).
            hIcon: LoadIconW(Some(hinst), windows::core::PCWSTR(1 as *const u16)).unwrap_or_default(),
            ..Default::default()
        };
        // Falha a partir da segunda vez (classe já existe): tudo bem.
        RegisterClassW(&wc);

        // No monitor do mouse (onde está o dock), centrada na área de trabalho.
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let monitor = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut mi);
        let (mut dpi, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dy);
        let s = dpi as f32 / 96.0;
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut r = RECT { left: 0, top: 0, right: (WIDTH * s).round() as i32, bottom: (HEIGHT * s).round() as i32 };
        let _ = AdjustWindowRectExForDpi(&mut r, style, false, WINDOW_EX_STYLE(0), dpi);
        let (ww, wh) = (r.right - r.left, r.bottom - r.top);
        let work = mi.rcWork;
        let (x, y) = ((work.left + work.right - ww) / 2, (work.top + work.bottom - wh) / 2);
        let title = HSTRING::from(tr("Configurações do ArcDockWin"));
        let Ok(hwnd) = CreateWindowExW(WINDOW_EX_STYLE(0), class, &title, style, x, y, ww, wh, None, None, Some(hinst), None)
        else {
            return;
        };
        let scale = GetDpiForWindow(hwnd) as f32 / 96.0;
        let Ok(text) = Text::new(scale) else {
            let _ = DestroyWindow(hwnd);
            return;
        };
        apply_theme(hwnd);
        let mut win = Win {
            hwnd,
            scale,
            text,
            buf: None,
            page: 0,
            scroll: 0.0,
            max_scroll: 0.0,
            zones: Vec::new(),
            hot: None,
            pressed: None,
            drag: None,
            focus: None,
            tracking: false,
        };
        win.redraw();
        WIN.with(|w| *w.borrow_mut() = Some(win));
        HWND_OPEN.store(hwnd.0 as isize, Ordering::Relaxed);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                with_win(|w| {
                    if let Some(b) = &w.buf {
                        let _ = BitBlt(hdc, 0, 0, b.w, b.h, Some(b.memdc), 0, 0, SRCCOPY);
                    }
                });
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_MOUSEWHEEL => {
                let steps = ((wp.0 >> 16) as u16 as i16) as f32 / WHEEL_DELTA as f32;
                with_win(|w| {
                    let next = (w.scroll - steps * 64.0 * w.scale).clamp(0.0, w.max_scroll);
                    if next != w.scroll {
                        w.scroll = next;
                        w.hot = None;
                        w.redraw();
                    }
                });
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let (x, slide_to) = (lp, with_win(|w| w.drag_value(w.hit(lp).0)).flatten());
                if let Some((o, v)) = slide_to {
                    slide(o, v, false);
                }
                with_win(|w| {
                    if !w.tracking {
                        w.tracking = true;
                        let mut tme = TRACKMOUSEEVENT {
                            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        let _ = TrackMouseEvent(&mut tme);
                    }
                    let hot = w.hit(x).1;
                    if hot != w.hot || slide_to.is_some() {
                        w.hot = hot;
                        w.redraw();
                    }
                });
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                with_win(|w| {
                    w.tracking = false;
                    if w.hot.take().is_some() {
                        w.redraw();
                    }
                });
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let slide_to = with_win(|w| {
                    w.focus = None;
                    let (x, i) = w.hit(lp);
                    SetCapture(hwnd);
                    match i.map(|i| (i, w.zones[i].act)) {
                        Some((_, Act::Slider(o, l, r))) => {
                            w.drag = Some((o, l, r));
                            w.drag_value(x)
                        }
                        Some((i, _)) => {
                            w.pressed = Some(i);
                            None
                        }
                        None => None,
                    }
                })
                .flatten();
                if let Some((o, v)) = slide_to {
                    slide(o, v, false);
                }
                with_win(|w| w.redraw());
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let _ = ReleaseCapture();
                let done = with_win(|w| {
                    if let Some((o, _, _)) = w.drag.take() {
                        return Some((Act::Slider(o, 0.0, 0.0), true));
                    }
                    let i = w.pressed.take()?;
                    (w.hit(lp).1 == Some(i)).then(|| (w.zones[i].act, false))
                })
                .flatten();
                match done {
                    // Fim do arrasto: recarrega os ícones no tamanho novo.
                    Some((Act::Slider(o, ..), true)) => slide(o, value(o), true),
                    Some((act, _)) => run(act),
                    None => {}
                }
                with_win(|w| w.redraw());
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let act = match with_win(|w| w.key(wp.0 as u16)).flatten() {
                    Some((Act::Slider(o, ..), v)) => {
                        slide(o, v, true);
                        None
                    }
                    Some((act, _)) => {
                        run(act);
                        Some(act)
                    }
                    None => return LRESULT(0),
                };
                with_win(|w| {
                    w.redraw();
                    // A lista pode ter mudado (módulo subiu/desceu): o foco segue a mesma ação.
                    if let Some(i) = act.and_then(|a| w.zones.iter().position(|z| z.act == a))
                        && w.focus != Some(i)
                    {
                        w.focus = Some(i);
                        w.redraw();
                    }
                });
                LRESULT(0)
            }
            WM_DPICHANGED => {
                // Outro monitor (ou escala mudou): fontes na escala nova e o tamanho sugerido.
                let scale = (wp.0 & 0xffff) as f32 / 96.0;
                if let Ok(text) = Text::new(scale) {
                    with_win(|w| {
                        w.scale = scale;
                        w.text = text;
                    });
                }
                let r = &*(lp.0 as *const RECT);
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                with_win(|w| w.redraw());
                LRESULT(0)
            }
            WM_SETTINGCHANGE => {
                // Pode ser a troca de tema claro/escuro.
                apply_theme(hwnd);
                with_win(|w| w.redraw());
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_DESTROY => {
                HWND_OPEN.store(0, Ordering::Relaxed);
                let old = WIN.with(|w| w.try_borrow_mut().ok().and_then(|mut g| g.take()));
                drop(old);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
