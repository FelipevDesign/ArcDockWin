//! Layout com zoom estilo macOS e desenho do dock.

use std::f32::consts::PI;

use tiny_skia::{
    Color, FillRule, FilterQuality, Paint, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke,
    Transform,
};

use crate::apps::App;

pub struct Metrics {
    pub base: f32,   // tamanho normal do ícone
    pub max: f32,    // tamanho máximo com zoom
    pub gap: f32,    // espaço entre ícones
    pub sep: f32,    // espaço extra do separador (fixados | abertos)
    pub pad: f32,    // espaço interno do fundo
    pub margin: f32, // distância até a borda de baixo
    pub range: f32,  // raio de alcance do zoom
    pub radius: f32, // arredondamento do fundo
    pub dot: f32,    // raio do indicador de app aberto
}

impl Metrics {
    pub fn new(scale: f32) -> Self {
        let (base, gap) = (48.0 * scale, 9.0 * scale);
        Self {
            base,
            max: base * 1.35,
            gap,
            sep: 10.0 * scale, // espaçamento simétrico em torno do divisor
            pad: 8.0 * scale,
            margin: 6.0 * scale,
            range: 2.0 * (base + gap), // 2 slots de cada lado
            radius: 20.0 * scale,
            dot: 1.7 * scale,
        }
    }

    pub fn window_height(&self) -> i32 {
        (self.margin + self.base + 2.0 * self.pad + (self.max - self.base) + self.pad).ceil() as i32
    }
}

pub struct Theme {
    tint: [u8; 4],
    border: [u8; 4],
    dot: [u8; 4],
    divider: [u8; 4],
    /// Pontinhos do app em foco: cor de destaque do Windows.
    accent: [u8; 4],
}

impl Theme {
    pub fn new(light: bool, accent: Option<[u8; 4]>) -> Self {
        let (tint, border, dot, divider) = if light {
            ([250, 250, 252, 108], [255, 255, 255, 178], [0, 0, 0, 170], [0, 0, 0, 45])
        } else {
            ([40, 40, 44, 116], [255, 255, 255, 58], [255, 255, 255, 210], [255, 255, 255, 60])
        };
        Self { tint, border, dot, divider, accent: accent.unwrap_or(dot) }
    }
}

pub struct Slot {
    pub x: f32,
    pub size: f32,
    /// Deslocamento só do desenho do ícone (arrastado), sem mexer no fundo nem no separador.
    pub dx: f32,
}

/// Posição e tamanho de cada ícone dado o mouse em `mx` e a intensidade do hover (0..1).
/// `seps`: índices dos itens com separador depois (último fixado; último app antes das pilhas).
pub fn layout(n: usize, seps: &[usize], width: f32, m: &Metrics, mx: f32, hover: f32) -> Vec<Slot> {
    if n == 0 {
        return Vec::new();
    }
    let extra = |i: usize| m.sep * seps.iter().filter(|&&s| i > s).count() as f32;
    let base_total = n as f32 * m.base + (n - 1) as f32 * m.gap + extra(n - 1);
    let base_left = (width - base_total) / 2.0;

    let sizes: Vec<f32> = (0..n)
        .map(|i| {
            let center = base_left + i as f32 * (m.base + m.gap) + extra(i) + m.base / 2.0;
            let d = (mx - center).abs();
            let f = if d < m.range { 0.5 * (1.0 + (PI * d / m.range).cos()) } else { 0.0 };
            m.base + (m.max - m.base) * f * hover
        })
        .collect();

    let total = sizes.iter().sum::<f32>() + (n - 1) as f32 * m.gap + extra(n - 1);
    let mut x = (width - total) / 2.0;
    sizes
        .into_iter()
        .enumerate()
        .map(|(i, size)| {
            let slot = Slot { x, size, dx: 0.0 };
            x += size + m.gap;
            if seps.contains(&i) {
                x += m.sep;
            }
            slot
        })
        .collect()
}

/// Retângulo do fundo (esquerda, topo, direita, base), em pixels inteiros.
pub fn box_rect(slots: &[Slot], m: &Metrics, height: f32) -> Option<(i32, i32, i32, i32)> {
    let (first, last) = (slots.first()?, slots.last()?);
    let bottom = height - m.margin;
    Some((
        (first.x - m.pad).round() as i32,
        (bottom - m.base - 2.0 * m.pad).round() as i32,
        (last.x + last.size + m.pad).round() as i32,
        bottom.round() as i32,
    ))
}

pub fn draw(
    pm: &mut Pixmap,
    slots: &[Slot],
    seps: &[usize],
    apps: &[&App],
    m: &Metrics,
    theme: &Theme,
    hover: f32,
    focus: Option<usize>,
) {
    pm.fill(Color::TRANSPARENT);
    let Some((l, t, r, b)) = box_rect(slots, m, pm.height() as f32) else {
        return;
    };
    let (l, t, r, b) = (l as f32, t as f32, r as f32, b as f32);
    let icon_bottom = b - m.pad;
    let id = Transform::identity();

    // Área quase invisível acima do fundo: mantém o mouse "dentro" do dock enquanto há zoom
    // (pixels com alfa 0 deixam o clique passar para a janela de trás).
    if hover > 0.01 {
        if let Some(rect) = Rect::from_ltrb(l, 0.0, r, t) {
            pm.fill_rect(rect, &solid([0, 0, 0, 1]), id, None);
        }
    }

    // Vidro: o desfoque vem da janela de fundo; aqui só a tinta e a borda.
    if let Some(bg) = rounded_rect(l, t, r - l, b - t, m.radius) {
        pm.fill_path(&bg, &solid(theme.tint), FillRule::Winding, id, None);
    }
    if let Some(border) = rounded_rect(l + 0.5, t + 0.5, r - l - 1.0, b - t - 1.0, m.radius - 0.5) {
        let stroke = Stroke { width: 1.0, ..Default::default() };
        pm.stroke_path(&border, &solid(theme.border), &stroke, id, None);
    }

    for s in seps.iter().filter_map(|&s| slots.get(s)) {
        let x = (s.x + s.size + (m.gap + m.sep) / 2.0).round() + 0.5;
        if let Some(rect) = Rect::from_xywh(x - 0.5, t + m.pad * 1.3, 1.0, b - t - m.pad * 2.6) {
            pm.fill_rect(rect, &solid(theme.divider), id, None);
        }
    }

    let icon_paint = PixmapPaint { quality: FilterQuality::Bicubic, ..Default::default() };
    for (i, (slot, app)) in slots.iter().zip(apps).enumerate() {
        // Ícone arrastado: desenhado fora do lugar (o espaço dele continua no layout).
        let sx = slot.x + slot.dx;
        // App pedindo atenção: pula no espaço livre acima (até meio ícone).
        let (lift, alert) = crate::attention::state(&app.windows);
        let y = icon_bottom - slot.size;
        let y = y - lift * y.min(slot.size * 0.5);
        match &app.icon {
            // O ícone como ele é, ocupando o espaço do app (sem caixa por trás).
            Some(icon) => {
                let (iw, ih) = (icon.width() as f32, icon.height() as f32);
                let s = slot.size / iw.max(ih);
                let ox = sx + (slot.size - iw * s) / 2.0;
                let oy = y + (slot.size - ih * s) / 2.0;
                let tr = Transform::from_row(s, 0.0, 0.0, s, ox, oy);
                pm.draw_pixmap(0, 0, icon.as_ref(), &icon_paint, tr, None);
            }
            None => {
                if let Some(p) = rounded_rect(sx, y, slot.size, slot.size, slot.size * 0.22) {
                    pm.fill_path(&p, &solid([120, 120, 128, 220]), FillRule::Winding, id, None);
                }
            }
        }
        if alert {
            let r = m.dot * 2.5;
            if let Some(badge) = PathBuilder::from_circle(sx + slot.size - r, y + r, r) {
                pm.fill_path(&badge, &solid([255, 59, 48, 255]), FillRule::Winding, id, None);
            }
        }
        // macOS-style single running-app indicator; the focused app uses the system accent.
        if !app.windows.is_empty() {
            let color = if focus == Some(i) { theme.accent } else { theme.dot };
            let w = m.dot * 3.6;
            let h = m.dot * 1.5;
            let x = sx + slot.size / 2.0 - w / 2.0;
            let y = icon_bottom + m.pad / 2.0 + 0.5 - h / 2.0;
            if let Some(indicator) = rounded_rect(x, y, w, h, h / 2.0) {
                pm.fill_path(&indicator, &solid(color), FillRule::Winding, id, None);
            }
        }
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
