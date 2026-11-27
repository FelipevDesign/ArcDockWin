//! Ferramentas na ponta direita do dock (antes de Downloads e Lixeira), cada uma ligada nas
//! Configurações: prateleira, histórico da área de transferência, coletor de cores, timer de
//! foco e música. Também os grupos de apps, modos de trabalho, pastas como pilha e o painel de
//! editar um atalho fixado. Os ícones das ferramentas são glifos discretos num quadrado
//! translúcido; nada roda em segundo plano com a ferramenta desligada.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tiny_skia::{FillRule, Pixmap, PixmapMut, PixmapPaint, Stroke, Transform};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT_CENTER;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, KillTimer, MB_ICONASTERISK, MF_GRAYED, MF_SEPARATOR, MF_STRING,
    SetForegroundWindow, SetTimer, TPM_BOTTOMALIGN, TPM_CENTERALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::apps::{self, App};
use crate::extras;
use crate::i18n::tr;
use crate::panel::{self, Buffer, Event, Font, Item, PanelContent, Text, TextOp, Widget};
use crate::topbar::rounded_rect;

pub const SHELF: &str = "::tool-shelf";
pub const CLIP: &str = "::tool-clip";
pub const PICKER: &str = "::tool-picker";
pub const TIMER: &str = "::tool-timer";
pub const MUSIC: &str = "::tool-music";
/// Duas ou mais ferramentas juntas num lugar só do dock.
pub const GRID: &str = "::tool-grid";

/// Ferramentas da grade, na ordem das células (cima-esquerda, cima-direita, baixo...).
pub fn grid_tools() -> Vec<&'static str> {
    enabled()
}

/// Ferramentas na ordem do dock, com a chave que as liga.
const ALL: [(&str, &str); 5] =
    [(SHELF, "tool_shelf"), (CLIP, "tool_clip"), (PICKER, "tool_picker"), (TIMER, "tool_timer"), (MUSIC, "tool_music")];

/// Ferramentas que aparecem no dock agora. A prateleira só aparece com algo guardado (ela
/// se enche soltando arquivos no dock); a música fica num quadro próprio (musicbar.rs).
pub fn enabled() -> Vec<&'static str> {
    ALL.iter()
        .filter(|(_, k)| extras::on(k))
        .map(|(s, _)| *s)
        .filter(|s| *s != MUSIC && (*s != SHELF || (!shelf().is_empty() && !crate::droptarget::dragging())))
        .collect()
}

pub fn is_tool(source: &str) -> bool {
    source.starts_with("::tool-")
}

pub fn name(source: &str) -> String {
    let n = match source {
        SHELF => "Prateleira",
        CLIP => "Área de transferência",
        GRID => "Ferramentas",
        PICKER => "Coletor de cores",
        TIMER => return timer_name(),
        MUSIC => {
            return crate::media::current()
                .map(|m| if m.artist.is_empty() { m.title } else { format!("{} · {}", m.title, m.artist) })
                .unwrap_or_else(|| tr("Música"));
        }
        _ => "",
    };
    tr(n)
}

/// Tema das ferramentas: o do dock (o estilo escolhido vale mais que o do Windows).
pub fn light_style() -> bool {
    light()
}

fn light() -> bool {
    match extras::choice("fx_style") {
        1 => false,
        2 => true,
        _ => crate::light_theme(),
    }
}

// ---------------------------------------------------------------------------------------------
// Ícones

/// Ícone discreto: só o glifo, menor que um app e sem fundo (e um número pequeno embaixo,
/// se houver). `strong` = em destaque (timer acabou); `faded` = pausado.
fn glyph_icon(glyph: u16, caption: Option<&str>, px: i32, strong: bool) -> Option<Pixmap> {
    glyph_icon_ex(glyph, caption, px, strong, false)
}

fn glyph_icon_ex(glyph: u16, caption: Option<&str>, px: i32, strong: bool, faded: bool) -> Option<Pixmap> {
    let light = light();
    let buf = Buffer::new(px, px)?;
    let n = (px * px * 4) as usize;
    let bytes = unsafe { std::slice::from_raw_parts_mut(buf.bits, n) };
    bytes.fill(0);
    let text = Text::new(px as f32 / 62.0).ok()?;
    let a = if faded { 0.38 } else if strong { 0.95 } else { 0.7 };
    let ink = if strong {
        [0.38, 0.65, 0.98, a]
    } else if light {
        [0.12, 0.13, 0.16, a]
    } else {
        [1.0, 1.0, 1.0, a]
    };
    let f = px as f32;
    let mut ops = Vec::new();
    let gy = if caption.is_some() { -f * 0.08 } else { 0.0 };
    ops.push(TextOp {
        text: vec![glyph],
        font: Font::IconLarge,
        rect: [0.0, gy, f, f],
        align: DWRITE_TEXT_ALIGNMENT_CENTER,
        color: ink,
        wrap: false,
        clip: None,
    });
    if let Some(c) = caption {
        ops.push(TextOp {
            text: c.encode_utf16().collect(),
            font: Font::Small,
            rect: [0.0, f * 0.6, f, f * 0.3],
            align: DWRITE_TEXT_ALIGNMENT_CENTER,
            color: ink,
            wrap: false,
            clip: None,
        });
    }
    // Glifos de linha fina: o texto do DirectWrite vem com y no centro da caixa.
    let _ = (&rounded_rect, PixmapMut::from_bytes, Stroke::default);
    text.draw(buf.memdc, px, px, &ops);
    to_pixmap(bytes, px)
}

/// Duas ferramentas uma sobre a outra, ou três/quatro em grade 2×2, num lugar só.
fn grid_icon(px: i32) -> Option<Pixmap> {
    let list = grid_tools();
    let light = light();
    let buf = Buffer::new(px, px)?;
    let bytes = unsafe { std::slice::from_raw_parts_mut(buf.bits, (px * px * 4) as usize) };
    bytes.fill(0);
    let f = px as f32;
    let text = Text::new(f / 95.0).ok()?;
    let base = if light { [0.12, 0.13, 0.16] } else { [1.0, 1.0, 1.0] };
    let ops: Vec<TextOp> = list
        .iter()
        .enumerate()
        .map(|(k, t)| {
            let (glyph, a, strong) = match *t {
                SHELF => (0xE7B8, 0.72, false),
                CLIP => (0xE77F, if crate::clipboard::paused() { 0.35 } else { 0.72 }, false),
                PICKER => (0xE790, 0.72, false),
                TIMER => (0xE916, 0.8, timer_left().0.is_some() || timer_left().1),
                _ => (0xE8D6, 0.72, false),
            };
            let color = if strong { [0.38, 0.65, 0.98, 0.95] } else { [base[0], base[1], base[2], a] };
            let rect = if list.len() <= 2 {
                [0.0, k as f32 * f / 2.0, f, f / 2.0]
            } else {
                [(k % 2) as f32 * f / 2.0, (k / 2) as f32 * f / 2.0, f / 2.0, f / 2.0]
            };
            TextOp { text: vec![glyph], font: Font::IconLarge, rect, align: DWRITE_TEXT_ALIGNMENT_CENTER, color, wrap: false, clip: None }
        })
        .collect();
    text.draw(buf.memdc, px, px, &ops);
    to_pixmap(bytes, px)
}

/// BGRA pré-multiplicado (DIB) → Pixmap (RGBA pré-multiplicado).
fn to_pixmap(bgra: &[u8], px: i32) -> Option<Pixmap> {
    let mut pm = Pixmap::new(px as u32, px as u32)?;
    for (d, s) in pm.data_mut().chunks_exact_mut(4).zip(bgra.chunks_exact(4)) {
        d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
    }
    Some(pm)
}

/// Capa do que está tocando, com os cantos arredondados.
fn cover_icon(px: i32) -> Option<Pixmap> {
    let m = crate::media::current()?;
    let cover = m.cover?;
    let c = crate::media::COVER as i32;
    let src = to_pixmap(&cover, c)?;
    let mut pm = Pixmap::new(px as u32, px as u32)?;
    let k = px as f32 / c as f32;
    let mask = {
        let mut mask = tiny_skia::Mask::new(px as u32, px as u32)?;
        let p = rounded_rect(0.0, 0.0, px as f32, px as f32, px as f32 * 0.22)?;
        mask.fill_path(&p, FillRule::Winding, true, Transform::identity());
        mask
    };
    let paint = PixmapPaint { quality: tiny_skia::FilterQuality::Bicubic, ..Default::default() };
    pm.draw_pixmap(0, 0, src.as_ref(), &paint, Transform::from_scale(k, k), Some(&mask));
    // Pausado: um pouco apagado.
    if !m.playing {
        for p in pm.data_mut().chunks_exact_mut(4) {
            for v in p.iter_mut() {
                *v = (*v as f32 * 0.55) as u8;
            }
        }
    }
    Some(pm)
}

pub fn icon(source: &str, px: i32) -> Option<Pixmap> {
    match source {
        SHELF => {
            let n = shelf().len();
            glyph_icon(0xE7B8, (n > 0).then(|| n.to_string()).as_deref(), px, false)
        }
        CLIP => glyph_icon_ex(0xE77F, None, px, false, crate::clipboard::paused()),
        PICKER => glyph_icon(0xE790, None, px, false),
        TIMER => {
            let (left, done) = timer_left();
            let cap = left.map(|d| format!("{}", d.as_secs().div_ceil(60)));
            glyph_icon(0xE916, if done { Some("0") } else { cap.as_deref() }, px, done)
        }
        MUSIC => cover_icon(px).or_else(|| glyph_icon(0xE8D6, None, px, false)),
        GRID => grid_icon(px),
        _ => None,
    }
}

/// Ícone de grupo (até 4 ícones dos apps numa grade) ou de modo de trabalho (glifo).
pub fn group_icon(head: &str, members: &[String], px: i32) -> Option<Pixmap> {
    if apps::is_mode(head) {
        return glyph_icon(0xE821, None, px, false);
    }
    let light = light();
    let mut pm = Pixmap::new(px as u32, px as u32)?;
    let f = px as f32;
    let id = Transform::identity();
    let paint = |c: [u8; 4]| {
        let mut p = tiny_skia::Paint::default();
        p.set_color_rgba8(c[0], c[1], c[2], c[3]);
        p.anti_alias = true;
        p
    };
    if let Some(p) = rounded_rect(0.5, 0.5, f - 1.0, f - 1.0, f * 0.24) {
        let (bg, line) = if light { ([0, 0, 0, 20], [0, 0, 0, 38]) } else { ([255, 255, 255, 26], [255, 255, 255, 46]) };
        pm.fill_path(&p, &paint(bg), FillRule::Winding, id, None);
        pm.stroke_path(&p, &paint(line), &Stroke { width: 1.0, ..Default::default() }, id, None);
    }
    let cell = (f * 0.36).round() as i32;
    let pad = (f - 2.0 * cell as f32) / 3.0;
    for (k, m) in members.iter().take(4).enumerate() {
        let Some(ic) = apps::load_icon(m, cell) else { continue };
        let (cx, cy) = ((k % 2) as f32, (k / 2) as f32);
        let x = pad + cx * (cell as f32 + pad);
        let y = pad + cy * (cell as f32 + pad);
        let s = cell as f32 / ic.width().max(ic.height()) as f32;
        let ox = x + (cell as f32 - ic.width() as f32 * s) / 2.0;
        let oy = y + (cell as f32 - ic.height() as f32 * s) / 2.0;
        let paint = PixmapPaint { quality: tiny_skia::FilterQuality::Bicubic, ..Default::default() };
        pm.draw_pixmap(0, 0, ic.as_ref(), &paint, Transform::from_row(s, 0.0, 0.0, s, ox, oy), None);
    }
    Some(pm)
}

// ---------------------------------------------------------------------------------------------
// Relógio das ferramentas (timer e música): só existe com uma delas ligada.

static TICKER: AtomicUsize = AtomicUsize::new(0);
static TICKS: AtomicUsize = AtomicUsize::new(0);
static LAST_LABEL: Mutex<String> = Mutex::new(String::new());

pub fn sync() {
    let need = extras::on("tool_timer") || extras::on("tool_music");
    let id = TICKER.load(Ordering::Relaxed);
    unsafe {
        if need && id == 0 {
            let id = SetTimer(None, 0, 1000, Some(tick));
            TICKER.store(id, Ordering::Relaxed);
            if extras::on("tool_music") {
                crate::media::refresh(None);
            }
        } else if !need && id != 0 {
            let _ = KillTimer(None, id);
            TICKER.store(0, Ordering::Relaxed);
        }
    }
}

unsafe extern "system" fn tick(_: HWND, _: u32, _: usize, _: u32) {
    let n = TICKS.fetch_add(1, Ordering::Relaxed);
    let mut label = String::new();
    if extras::on("tool_timer") {
        let (left, done) = timer_left();
        if let Some(l) = left
            && l.is_zero()
        {
            finish_timer();
        }
        label.push_str(&format!("{:?}{done}", left.map(|d| d.as_secs().div_ceil(60))));
    }
    if extras::on("tool_music") {
        if n % 3 == 0 {
            crate::media::refresh(None);
        }
        if let Some(m) = crate::media::current() {
            label.push_str(&format!("{}{}{}", m.title, m.artist, m.playing));
        }
    }
    let changed = LAST_LABEL.lock().map(|mut l| std::mem::replace(&mut *l, label.clone()) != label).unwrap_or(false);
    if changed {
        crate::refresh_tools();
        crate::musicbar::update();
    }
    if changed || panel::is_open("timer") {
        panel::refresh();
    }
}

// ---------------------------------------------------------------------------------------------
// Timer de foco

struct Timer {
    end: Option<Instant>,
    /// Pausado: quanto faltava.
    paused: Option<Duration>,
    done: bool,
}

static TIMER_STATE: Mutex<Timer> = Mutex::new(Timer { end: None, paused: None, done: false });

/// Quanto falta (None = parado) e se acabou de terminar.
fn timer_left() -> (Option<Duration>, bool) {
    let Ok(t) = TIMER_STATE.lock() else { return (None, false) };
    let left = t.paused.or_else(|| t.end.map(|e| e.saturating_duration_since(Instant::now())));
    (left, t.done)
}

fn timer_name() -> String {
    match timer_left() {
        (_, true) => tr("Tempo esgotado"),
        (Some(d), _) => format!("{} {:02}:{:02}", tr("Foco:"), d.as_secs() / 60, d.as_secs() % 60),
        _ => tr("Timer de foco"),
    }
}

fn start_timer(min: u64) {
    if let Ok(mut t) = TIMER_STATE.lock() {
        *t = Timer { end: Some(Instant::now() + Duration::from_secs(min * 60)), paused: None, done: false };
    }
    crate::refresh_tools();
}

fn finish_timer() {
    if let Ok(mut t) = TIMER_STATE.lock() {
        *t = Timer { end: None, paused: None, done: true };
    }
    unsafe {
        let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(MB_ICONASTERISK);
    }
    crate::refresh_tools();
}

fn stop_timer() {
    if let Ok(mut t) = TIMER_STATE.lock() {
        *t = Timer { end: None, paused: None, done: false };
    }
    crate::refresh_tools();
}

fn pause_timer() {
    if let Ok(mut t) = TIMER_STATE.lock() {
        if let Some(left) = t.paused.take() {
            t.end = Some(Instant::now() + left);
        } else if let Some(e) = t.end.take() {
            t.paused = Some(e.saturating_duration_since(Instant::now()));
        }
    }
    crate::refresh_tools();
}

struct TimerPanel;

impl PanelContent for TimerPanel {
    fn centered(&self) -> bool {
        true
    }
    fn above(&self) -> bool {
        true
    }
    fn width(&self) -> f32 {
        260.0
    }
    fn build(&mut self) -> Vec<Widget> {
        let (left, done) = timer_left();
        let paused = TIMER_STATE.lock().map(|t| t.paused.is_some()).unwrap_or(false);
        let mut out = vec![Widget::Label(tr("Timer de foco"))];
        match left {
            Some(d) => {
                out.push(Widget::Title(format!("{:02}:{:02}", d.as_secs() / 60, d.as_secs() % 60)));
                out.push(Widget::Row { id: 2, glyph: Some(if paused { 0xE768 } else { 0xE769 }), text: tr(if paused { "Continuar" } else { "Pausar" }), sub: None, selected: false });
                out.push(Widget::Row { id: 3, glyph: Some(0xE710), text: tr("Mais 5 minutos"), sub: None, selected: false });
                out.push(Widget::Row { id: 4, glyph: Some(0xE71A), text: tr("Parar"), sub: None, selected: false });
            }
            None => {
                if done {
                    out.push(Widget::Text(tr("Tempo esgotado. Bom trabalho!")));
                }
                let def = extras::num("timer_min") as u64;
                let mut opts = vec![def, 15, 25, 50];
                opts.dedup();
                let mut seen = Vec::new();
                for m in opts {
                    if seen.contains(&m) {
                        continue;
                    }
                    seen.push(m);
                    out.push(Widget::Row { id: 100 + m as u32, glyph: Some(0xE916), text: format!("{} {m} min", tr("Começar")), sub: None, selected: false });
                }
            }
        }
        out
    }
    fn event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Click(2) => pause_timer(),
            Event::Click(3) => {
                if let Ok(mut t) = TIMER_STATE.lock() {
                    let add = Duration::from_secs(300);
                    if let Some(p) = t.paused.as_mut() {
                        *p += add;
                    } else if let Some(e) = t.end.as_mut() {
                        *e += add;
                    }
                }
            }
            Event::Click(4) => stop_timer(),
            Event::Click(id) if id >= 100 => {
                start_timer((id - 100) as u64);
                panel::close();
                return false;
            }
            _ => return false,
        }
        true
    }
}

// ---------------------------------------------------------------------------------------------
// Prateleira

static SHELF_ITEMS: Mutex<Option<Vec<PathBuf>>> = Mutex::new(None);

fn shelf_file() -> PathBuf {
    apps::config_path().with_file_name("prateleira.txt")
}

fn shelf() -> Vec<PathBuf> {
    let Ok(mut s) = SHELF_ITEMS.lock() else { return Vec::new() };
    s.get_or_insert_with(|| {
        std::fs::read_to_string(shelf_file())
            .unwrap_or_default()
            .lines()
            .map(PathBuf::from)
            .filter(|p| p.exists())
            .collect()
    })
    .clone()
}

fn set_shelf(list: Vec<PathBuf>) {
    let text: String = list.iter().map(|p| format!("{}\n", p.to_string_lossy())).collect();
    let _ = std::fs::write(shelf_file(), text);
    let empty = list.is_empty();
    if let Ok(mut s) = SHELF_ITEMS.lock() {
        *s = Some(list);
    }
    if empty && panel::is_open("shelf") {
        panel::close();
    }
    // A prateleira aparece/some do dock conforme tem algo guardado.
    crate::apply_settings(false);
    crate::refresh_tools();
}

/// Arquivos soltos no ícone da prateleira (ou vindos da área de transferência).
pub fn shelf_add(paths: &[PathBuf]) {
    let mut list = shelf();
    for p in paths {
        list.retain(|x| x != p);
        list.insert(0, p.clone());
    }
    list.truncate(extras::num("shelf_limit") as usize);
    set_shelf(list);
}

const ROW: u32 = 1000;

struct ShelfPanel {
    icons: Vec<Option<Pixmap>>,
}

impl PanelContent for ShelfPanel {
    fn centered(&self) -> bool {
        true
    }
    fn above(&self) -> bool {
        true
    }
    fn build(&mut self) -> Vec<Widget> {
        let list = shelf();
        if self.icons.len() != list.len() {
            let px = (24.0 * crate::panel_scale()).round() as i32;
            self.icons = list.iter().map(|p| apps::load_icon(&p.to_string_lossy(), px)).collect();
        }
        let mut out = vec![Widget::Label(tr("Prateleira"))];
        if list.is_empty() {
            out.push(Widget::Text(tr("Solte arquivos no dock para guardá-los aqui.")));
        } else {
            let items = list
                .iter()
                .enumerate()
                .map(|(i, p)| Item {
                    id: ROW + i as u32,
                    icon: self.icons.get(i).cloned().flatten(),
                    glyph: 0,
                    text: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    hint: String::new(),
                })
                .collect();
            out.push(Widget::List { items, rows: list.len().min(8), selected: None, scroll: None, big: false });
            out.push(Widget::Text(tr("Clique para abrir; botão direito tira da prateleira.")));
        }
        out.push(Widget::Separator);
        if crate::clipboard::has_files() {
            out.push(Widget::Row { id: 4, glyph: Some(0xE77F), text: tr("Guardar arquivos copiados"), sub: None, selected: false });
        }
        if !list.is_empty() {
            out.push(Widget::Row { id: 2, glyph: Some(0xE8C8), text: tr("Copiar todos (para colar no Explorador)"), sub: None, selected: false });
            out.push(Widget::Row { id: 3, glyph: Some(0xE74D), text: tr("Esvaziar a prateleira"), sub: None, selected: false });
        }
        out
    }
    fn event(&mut self, ev: Event) -> bool {
        let list = shelf();
        match ev {
            Event::Click(id) if id >= ROW => {
                if let Some(p) = list.get((id - ROW) as usize) {
                    open_path(&p.to_string_lossy());
                    panel::close();
                }
                false
            }
            Event::RightClick(id) if id >= ROW => {
                let mut l = list;
                if ((id - ROW) as usize) < l.len() {
                    l.remove((id - ROW) as usize);
                    set_shelf(l);
                    self.icons.clear();
                }
                true
            }
            Event::Click(2) => {
                crate::clipboard::set_files(&list);
                panel::close();
                false
            }
            Event::Click(3) => {
                set_shelf(Vec::new());
                self.icons.clear();
                true
            }
            Event::Click(4) => {
                shelf_add(&crate::clipboard::files());
                self.icons.clear();
                true
            }
            _ => false,
        }
    }
}

fn open_path(p: &str) {
    unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), &HSTRING::from(p), None, None, windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL);
    }
}

// ---------------------------------------------------------------------------------------------
// Área de transferência

struct ClipPanel;

impl PanelContent for ClipPanel {
    fn centered(&self) -> bool {
        true
    }
    fn above(&self) -> bool {
        true
    }
    fn width(&self) -> f32 {
        360.0
    }
    fn build(&mut self) -> Vec<Widget> {
        let list = crate::clipboard::items();
        let mut out = vec![Widget::Label(tr("Área de transferência"))];
        if list.is_empty() {
            out.push(Widget::Text(tr("O que você copiar (texto) aparece aqui.")));
        } else {
            let items = list
                .iter()
                .enumerate()
                .take(12)
                .map(|(i, (t, pinned))| Item {
                    id: ROW + i as u32,
                    icon: None,
                    glyph: if *pinned { 0xE718 } else { 0xE8C8 },
                    text: t.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(80).collect(),
                    hint: String::new(),
                })
                .collect();
            out.push(Widget::List { items, rows: list.len().min(12), selected: None, scroll: None, big: false });
            out.push(Widget::Text(tr("Clique para copiar de novo; botão direito fixa ou solta.")));
        }
        out.push(Widget::Separator);
        let paused = crate::clipboard::paused();
        out.push(Widget::Row {
            id: 2,
            glyph: Some(if paused { 0xE768 } else { 0xE769 }),
            text: tr(if paused { "Retomar o histórico" } else { "Pausar o histórico" }),
            sub: None,
            selected: false,
        });
        out.push(Widget::Row { id: 3, glyph: Some(0xE74D), text: tr("Limpar (menos os fixados)"), sub: None, selected: false });
        out
    }
    fn event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Click(id) if id >= ROW => {
                crate::clipboard::reuse((id - ROW) as usize);
                panel::close();
                false
            }
            Event::RightClick(id) if id >= ROW => {
                crate::clipboard::toggle_pin((id - ROW) as usize);
                true
            }
            Event::Click(2) => {
                crate::clipboard::set_paused(!crate::clipboard::paused());
                crate::refresh_tools();
                true
            }
            Event::Click(3) => {
                crate::clipboard::clear();
                true
            }
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Música

struct MusicPanel;

impl PanelContent for MusicPanel {
    fn centered(&self) -> bool {
        true
    }
    fn above(&self) -> bool {
        true
    }
    fn width(&self) -> f32 {
        280.0
    }
    fn build(&mut self) -> Vec<Widget> {
        let mut out = vec![Widget::Label(tr("Tocando agora"))];
        match crate::media::current() {
            Some(m) => {
                out.push(Widget::Heading { text: m.title.clone(), buttons: Vec::new() });
                if !m.artist.is_empty() {
                    out.push(Widget::Text(m.artist.clone()));
                }
                out.push(Widget::Separator);
                out.push(Widget::Row { id: 1, glyph: Some(0xE892), text: tr("Anterior"), sub: None, selected: false });
                out.push(Widget::Row {
                    id: 2,
                    glyph: Some(if m.playing { 0xE769 } else { 0xE768 }),
                    text: tr(if m.playing { "Pausar" } else { "Tocar" }),
                    sub: None,
                    selected: false,
                });
                out.push(Widget::Row { id: 3, glyph: Some(0xE893), text: tr("Próxima"), sub: None, selected: false });
            }
            None => out.push(Widget::Text(tr("Nada tocando agora."))),
        }
        out
    }
    fn event(&mut self, ev: Event) -> bool {
        use crate::media::Action;
        let a = match ev {
            Event::Click(1) => Action::Previous,
            Event::Click(2) => Action::PlayPause,
            Event::Click(3) => Action::Next,
            _ => return false,
        };
        crate::media::refresh(Some(a));
        false
    }
}

// ---------------------------------------------------------------------------------------------
// Coletor de cores: a paleta das últimas cores fica no painel do botão direito.

struct Palette;

impl PanelContent for Palette {
    fn centered(&self) -> bool {
        true
    }
    fn above(&self) -> bool {
        true
    }
    fn width(&self) -> f32 {
        260.0
    }
    fn build(&mut self) -> Vec<Widget> {
        let colors = crate::picker::palette();
        let mut out = vec![Widget::Label(tr("Cores recentes"))];
        if colors.is_empty() {
            out.push(Widget::Text(tr("Clique no coletor e depois em qualquer ponto da tela.")));
        } else {
            let px = (22.0 * crate::panel_scale()).round() as u32;
            let items = colors
                .iter()
                .enumerate()
                .map(|(i, c)| Item {
                    id: ROW + i as u32,
                    icon: swatch(*c, px),
                    glyph: 0,
                    text: crate::picker::format(*c),
                    hint: crate::picker::format_other(*c),
                })
                .collect();
            out.push(Widget::List { items, rows: colors.len(), selected: None, scroll: None, big: false });
        }
        out.push(Widget::Separator);
        out.push(Widget::Row { id: 2, glyph: Some(0xE790), text: tr("Capturar uma cor"), sub: None, selected: false });
        if !colors.is_empty() {
            out.push(Widget::Row { id: 3, glyph: Some(0xE74D), text: tr("Limpar a paleta"), sub: None, selected: false });
        }
        out
    }
    fn event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Click(id) if id >= ROW => {
                if let Some(c) = crate::picker::palette().get((id - ROW) as usize) {
                    crate::clipboard::set_text(&crate::picker::format(*c));
                }
                panel::close();
                false
            }
            Event::Click(2) => {
                panel::close();
                crate::picker::start();
                false
            }
            Event::Click(3) => {
                crate::picker::clear();
                true
            }
            _ => false,
        }
    }
}

fn swatch(c: [u8; 3], px: u32) -> Option<Pixmap> {
    let mut pm = Pixmap::new(px, px)?;
    let p = rounded_rect(0.5, 0.5, px as f32 - 1.0, px as f32 - 1.0, px as f32 * 0.25)?;
    let mut paint = tiny_skia::Paint::default();
    paint.set_color_rgba8(c[0], c[1], c[2], 255);
    paint.anti_alias = true;
    pm.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
    Some(pm)
}

// ---------------------------------------------------------------------------------------------
// Clique e menu das ferramentas

fn anchor_rect(anchor: POINT) -> RECT {
    RECT { left: anchor.x, top: anchor.y, right: anchor.x, bottom: anchor.y }
}

/// Abre (ou fecha, no segundo clique) o painel `key` acima do ícone.
fn toggle_panel(key: &'static str, anchor: POINT, make: impl FnOnce() -> Box<dyn PanelContent>) {
    if panel::is_open(key) {
        panel::close();
        return;
    }
    panel::toggle(key, anchor_rect(anchor), make);
}

pub fn click(source: &str, anchor: POINT) {
    match source {
        SHELF => toggle_panel("shelf", anchor, || Box::new(ShelfPanel { icons: Vec::new() })),
        CLIP => toggle_panel("clip", anchor, || Box::new(ClipPanel)),
        PICKER => crate::picker::start(),
        TIMER => {
            if timer_left().1 {
                stop_timer();
            }
            toggle_panel("timer", anchor, || Box::new(TimerPanel));
        }
        MUSIC => {
            crate::media::refresh(None);
            toggle_panel("music", anchor, || Box::new(MusicPanel));
        }
        _ => {}
    }
}

pub fn context_menu(hwnd: HWND, source: &str, anchor: POINT) {
    match source {
        PICKER => {
            toggle_panel("palette", anchor, || Box::new(Palette));
            return;
        }
        SHELF | CLIP | TIMER | MUSIC => {}
        _ => return,
    }
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let add = |id: usize, t: &str, on: bool| {
            let t = HSTRING::from(tr(t));
            let _ = AppendMenuW(menu, if on { MF_STRING } else { MF_STRING | MF_GRAYED }, id, &t);
        };
        match source {
            SHELF => {
                add(1, "Esvaziar a prateleira", !shelf().is_empty());
            }
            CLIP => {
                add(2, if crate::clipboard::paused() { "Retomar o histórico" } else { "Pausar o histórico" }, true);
                add(3, "Limpar (menos os fixados)", true);
            }
            TIMER => {
                let running = timer_left().0.is_some();
                add(4, &format!("{} {} min", tr("Começar"), extras::num("timer_min")), !running);
                add(5, "Parar", running);
            }
            _ => {
                add(6, "Tocar / pausar", crate::media::current().is_some());
                add(7, "Próxima", crate::media::current().is_some());
            }
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        add(9, "Configurações…", true);
        let _ = SetForegroundWindow(hwnd);
        let flags = TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_CENTERALIGN;
        let cmd = TrackPopupMenu(menu, flags, anchor.x, anchor.y, None, hwnd, None);
        let _ = DestroyMenu(menu);
        match cmd.0 {
            1 => set_shelf(Vec::new()),
            2 => {
                crate::clipboard::set_paused(!crate::clipboard::paused());
                crate::refresh_tools();
            }
            3 => crate::clipboard::clear(),
            4 => start_timer(extras::num("timer_min") as u64),
            5 => stop_timer(),
            6 => crate::media::refresh(Some(crate::media::Action::PlayPause)),
            7 => crate::media::refresh(Some(crate::media::Action::Next)),
            9 => crate::settings::open(),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Grupos, modos de trabalho e pastas

/// Grupo: lista dos apps acima do ícone; clique abre o app.
struct GroupPanel {
    name: String,
    members: Vec<String>,
    icons: Vec<Option<Pixmap>>,
}

impl PanelContent for GroupPanel {
    fn centered(&self) -> bool {
        true
    }
    fn above(&self) -> bool {
        true
    }
    fn build(&mut self) -> Vec<Widget> {
        if self.icons.len() != self.members.len() {
            let px = (24.0 * crate::panel_scale()).round() as i32;
            self.icons = self.members.iter().map(|m| apps::load_icon(apps::line_source(m), px)).collect();
        }
        let items = self
            .members
            .iter()
            .enumerate()
            .map(|(i, m)| Item {
                id: ROW + i as u32,
                icon: self.icons[i].clone(),
                glyph: 0,
                text: apps::display_name(apps::line_source(m)),
                hint: String::new(),
            })
            .collect();
        let mut out = vec![Widget::Label(self.name.clone())];
        if self.members.is_empty() {
            out.push(Widget::Text(tr("Grupo vazio. Use o botão direito num app fixado para adicioná-lo.")));
        } else {
            out.push(Widget::List { items, rows: self.members.len().min(10), selected: None, scroll: None, big: false });
            out.push(Widget::Separator);
            out.push(Widget::Row { id: 2, glyph: Some(0xE8A7), text: tr("Abrir todos"), sub: None, selected: false });
        }
        out
    }
    fn event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Click(id) if id >= ROW => {
                if let Some(m) = self.members.get((id - ROW) as usize) {
                    launch_member(m);
                }
                panel::close();
                false
            }
            Event::Click(2) => {
                self.members.iter().for_each(|m| launch_member(m));
                panel::close();
                false
            }
            _ => false,
        }
    }
}

/// Item de grupo ou modo: `caminho` ou `caminho ; parâmetros`.
fn launch_member(m: &str) {
    let (src, args) = m.split_once(';').map(|(a, b)| (a.trim(), b.trim())).unwrap_or((m.trim(), ""));
    apps::launch(src, args);
}

pub fn open_group(a: &App, anchor: POINT) {
    let (name, members) = (apps::item_name(a), a.members.clone());
    if panel::is_open("group") {
        panel::close();
        return;
    }
    panel::toggle("group", anchor_rect(anchor), move || Box::new(GroupPanel { name, members, icons: Vec::new() }));
}

/// Modo de trabalho: abre todos os apps do modo de uma vez.
pub fn run_mode(a: &App) {
    for m in &a.members {
        launch_member(m);
    }
}

/// Pasta fixada como pilha: o conteúdo acima do ícone.
pub fn open_folder(path: &str, anchor: POINT) {
    crate::stacks::open_folder(Path::new(path), anchor);
}

// ---------------------------------------------------------------------------------------------
// Editar atalho / grupo / modo (botão direito)

const F_NAME: u32 = 1;
const F_ARGS: u32 = 2;

struct EditPin {
    source: String,
    name: String,
    args: String,
    special: bool,
    members: Vec<String>,
}

impl EditPin {
    fn save(&self) {
        if self.special {
            let prefix = if apps::is_group(&self.source) { apps::GROUP } else { apps::MODE };
            let name = if self.name.trim().is_empty() { tr("Grupo") } else { self.name.trim().to_string() };
            let a = App { source: format!("{prefix}{name}"), members: self.members.clone(), ..Default::default() };
            apps::replace_line(&self.source, Some(&apps::pin_line(&a)));
        } else {
            apps::edit_pin(&self.source, &self.args, &self.name);
        }
        crate::reload_pins();
    }
}

impl PanelContent for EditPin {
    fn centered(&self) -> bool {
        true
    }
    fn above(&self) -> bool {
        true
    }
    fn width(&self) -> f32 {
        360.0
    }
    fn build(&mut self) -> Vec<Widget> {
        let mut out = Vec::new();
        if self.special {
            out.push(Widget::Label(tr(if apps::is_group(&self.source) { "Editar grupo" } else { "Editar modo de trabalho" })));
            out.push(Widget::Field { id: F_NAME, placeholder: tr("Nome"), password: false });
            for (i, m) in self.members.iter().enumerate() {
                out.push(Widget::RowBadge {
                    id: ROW + i as u32,
                    glyph: None,
                    text: apps::display_name(apps::line_source(m)),
                    sub: None,
                    selected: false,
                    badge: 0xE711,
                });
            }
            if !self.members.is_empty() {
                out.push(Widget::Text(tr("Clique num item para tirá-lo.")));
            }
        } else {
            out.push(Widget::Label(tr("Editar atalho")));
            out.push(Widget::Text(self.source.clone()));
            out.push(Widget::Field { id: F_NAME, placeholder: tr("Nome no dock (opcional)"), password: false });
            out.push(Widget::Field { id: F_ARGS, placeholder: tr("Parâmetros (ex.: --perfil=2)"), password: false });
            out.push(Widget::Separator);
            out.push(Widget::Row { id: 3, glyph: Some(0xE91B), text: tr("Trocar ícone…"), sub: None, selected: false });
            out.push(Widget::Row { id: 4, glyph: Some(0xE777), text: tr("Voltar ao ícone original"), sub: None, selected: false });
        }
        out.push(Widget::Button { id: 5, text: tr("Salvar"), primary: true });
        out
    }
    fn event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Text(F_NAME, t) => self.name = t.to_string(),
            Event::Text(F_ARGS, t) => self.args = t.to_string(),
            Event::Submit(id, t) => {
                if id == F_NAME {
                    self.name = t.to_string();
                } else {
                    self.args = t.to_string();
                }
                self.save();
                panel::close();
            }
            Event::Click(id) if id >= ROW => {
                let i = (id - ROW) as usize;
                if i < self.members.len() {
                    self.members.remove(i);
                    return true;
                }
            }
            Event::Click(3) => {
                panel::close();
                if let Some(p) = pick_icon_file() {
                    apps::set_icon_override(&self.source, Some(&p));
                    crate::reload_pins();
                }
            }
            Event::Click(4) => {
                apps::set_icon_override(&self.source, None);
                panel::close();
                crate::reload_pins();
            }
            Event::Click(5) => {
                self.save();
                panel::close();
            }
            _ => {}
        }
        false
    }
}

pub fn edit(a: &App, anchor: POINT) {
    let special = apps::is_group(&a.source) || apps::is_mode(&a.source);
    let name = if special { apps::item_name(a) } else { a.label.clone() };
    panel::prefill(F_NAME, &name);
    panel::prefill(F_ARGS, &a.args);
    let e = EditPin { source: a.source.clone(), name, args: a.args.clone(), special, members: a.members.clone() };
    panel::toggle("edit", anchor_rect(anchor), move || Box::new(e));
}

/// Novo grupo ou modo com o app `source` (o grupo tira o app da lista de fixados).
pub fn new_group(source: &str, mode: bool) {
    let prefix = if mode { apps::MODE } else { apps::GROUP };
    let base = tr(if mode { "Modo de trabalho" } else { "Grupo" });
    let text = std::fs::read_to_string(apps::config_path()).unwrap_or_default();
    let mut n = 1;
    let mut name = base.clone();
    while text.lines().any(|l| apps::line_source(l) == format!("{prefix}{name}")) {
        n += 1;
        name = format!("{base} {n}");
    }
    let a = App { source: format!("{prefix}{name}"), members: vec![source.to_string()], ..Default::default() };
    if !mode {
        apps::replace_line(source, None);
    }
    apps::append_lines(&[apps::pin_line(&a)]);
    crate::reload_pins();
}

/// Põe o app `source` num grupo/modo existente (`target` = linha de origem do grupo).
pub fn add_to_group(source: &str, target: &str) {
    let text = std::fs::read_to_string(apps::config_path()).unwrap_or_default();
    let Some(line) = text.lines().find(|l| apps::line_source(l) == target) else { return };
    let new = format!("{} | {source}", line.trim());
    if apps::is_group(target) {
        apps::replace_line(source, None);
    }
    apps::replace_line(target, Some(&new));
    crate::reload_pins();
}

/// Desfaz um grupo: os apps voltam a ser fixados soltos.
pub fn ungroup(a: &App) {
    apps::replace_line(&a.source, None);
    apps::append_lines(&a.members);
    crate::reload_pins();
}

/// Grupos e modos fixados: (linha de origem, nome).
pub fn groups() -> Vec<(String, String)> {
    std::fs::read_to_string(apps::config_path())
        .unwrap_or_default()
        .lines()
        .map(apps::line_source)
        .filter(|l| apps::is_group(l) || apps::is_mode(l))
        .map(|l| (l.to_string(), l.trim_start_matches(':').split_once(' ').map(|x| x.1).unwrap_or(l).to_string()))
        .collect()
}

/// Escolher um arquivo de ícone (.ico, .exe, .dll, .png...).
fn pick_icon_file() -> Option<String> {
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let filters = [
            COMDLG_FILTERSPEC { pszName: w!("Ícones e imagens"), pszSpec: w!("*.ico;*.exe;*.dll;*.lnk;*.png;*.jpg") },
            COMDLG_FILTERSPEC { pszName: w!("Todos"), pszSpec: w!("*.*") },
        ];
        let _ = dlg.SetFileTypes(&filters);
        let _ = dlg.SetTitle(&HSTRING::from(tr("Escolha o ícone")));
        dlg.Show(None).ok()?;
        let item = dlg.GetResult().ok()?;
        let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as _));
        s
    }
}
