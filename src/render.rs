//! Layout com zoom estilo macOS e desenho do dock.

use std::f32::consts::PI;

use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, LinearGradient, Paint, PathBuilder, Pixmap, PixmapPaint, Point,
    Rect, SpreadMode, Stroke, Transform,
};

use crate::apps::App;

pub struct Metrics {
    pub base: f32,   // tamanho normal do ícone
    pub max: f32,    // tamanho máximo com zoom
    pub gap: f32,    // espaço entre ícones
    pub sep: f32,    // espaço extra do separador (fixados | abertos)
    pub pad: f32,    // espaço interno do fundo (em cima e embaixo)
    pub pad_x: f32,  // espaço interno nas pontas
    pub margin: f32, // distância até a borda de baixo
    pub range: f32,  // raio de alcance do zoom
    pub radius: f32, // arredondamento do fundo
    pub dot: f32,    // raio do indicador de app aberto
}

impl Metrics {
    /// Tamanho dos ícones e ampliação vêm das Configurações (config.rs).
    pub fn new(scale: f32) -> Self {
        use crate::config;
        let (base, gap) = (config::icon_size() * scale, config::icon_gap() * scale);
        let pad = config::dock_padding() * scale;
        let zoom = if config::magnify() { config::magnify_scale() } else { 1.0 };
        let pad_x = pad + config::dock_side_padding() * scale;
        // Ilhas: o espaço do separador vira o vão entre dois fundos.
        let sep = if crate::extras::on("fx_islands") {
            (crate::extras::num("fx_islands_gap") * scale + 2.0 * pad_x - gap).max(gap)
        } else {
            gap + 1.0 * scale
        };
        Self {
            base,
            max: base * zoom,
            gap,
            // Espaço extra em volta do divisor: o grupo fica bem separado, como no macOS.
            sep,
            pad,
            pad_x,
            margin: config::dock_margin() * scale,
            range: 2.0 * (base + gap), // 2 slots de cada lado
            // No máximo meia altura: o máximo vira uma pílula.
            radius: (config::dock_radius() * scale).min(base / 2.0 + pad),
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
    /// Brilho do vidro nas bordas de cima e de baixo (degradê por dentro).
    sheen: [u8; 4],
    /// Pontinhos do app em foco: cor de destaque do Windows.
    accent: [u8; 4],
    pub light: bool,
    /// Estilo "Mínimo": sem fundo nem vidro, só os ícones.
    pub minimal: bool,
}

impl Theme {
    pub fn new(light: bool, accent: Option<[u8; 4]>) -> Self {
        let style = crate::extras::choice("fx_style");
        let light = match style {
            1 => false,
            2 => true,
            _ => light,
        };
        let minimal = style == 3;
        // Opacidade da tinta: a das Configurações (padrão 50%).
        let alpha = (crate::config::dock_opacity() / 100.0 * 255.0).round() as u8;
        let (tint, border, dot, divider, sheen) = if light {
            ([250, 250, 252, alpha], [255, 255, 255, 178], [0, 0, 0, 170], [0, 0, 0, 45], [0, 0, 0, 0])
        } else {
            // Vidro escuro clássico (o brilho "sheen" fica zerado por padrão).
            ([40, 40, 44, alpha], [255, 255, 255, 58], [255, 255, 255, 210], [255, 255, 255, 60], [0, 0, 0, 0])
        };
        let (tint, border) = if minimal { ([0, 0, 0, 0], [0, 0, 0, 0]) } else { (tint, border) };
        // Opacidade dos indicadores de app aberto.
        let k = crate::extras::num("fx_dot_level") / 100.0;
        let fade = |c: [u8; 4]| [c[0], c[1], c[2], (c[3] as f32 * k).round() as u8];
        let accent = accent.unwrap_or(dot);
        Self { tint, border, dot: fade(dot), divider, sheen, accent: fade(accent), light, minimal }
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
    let left = |total: f32| (width - total) / 2.0;
    let base_left = left(base_total);

    let sizes: Vec<f32> = (0..n)
        .map(|i| {
            let center = base_left + i as f32 * (m.base + m.gap) + extra(i) + m.base / 2.0;
            let d = (mx - center).abs();
            let f = if d < m.range { 0.5 * (1.0 + (PI * d / m.range).cos()) } else { 0.0 };
            m.base + (m.max - m.base) * f * hover
        })
        .collect();

    let total = sizes.iter().sum::<f32>() + (n - 1) as f32 * m.gap + extra(n - 1);
    let mut x = left(total);
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
        (first.x - m.pad_x).round() as i32,
        (bottom - m.base - 2.0 * m.pad).round() as i32,
        (last.x + last.size + m.pad_x).round() as i32,
        bottom.round() as i32,
    ))
}

/// Fundos separados (modo ilhas): um retângulo por grupo entre separadores, ou nenhum.
pub fn islands(slots: &[Slot], seps: &[usize], m: &Metrics, height: f32) -> Vec<(i32, i32, i32, i32)> {
    if !crate::extras::on("fx_islands") || seps.is_empty() || slots.is_empty() {
        return Vec::new();
    }
    let bottom = height - m.margin;
    let top = (bottom - m.base - 2.0 * m.pad).round() as i32;
    let mut out = Vec::new();
    let mut start = 0;
    for i in 0..slots.len() {
        if seps.contains(&i) || i + 1 == slots.len() {
            let (a, b) = (&slots[start], &slots[i]);
            out.push(((a.x - m.pad_x).round() as i32, top, (b.x + b.size + m.pad_x).round() as i32, bottom.round() as i32));
            start = i + 1;
        }
    }
    out
}

/// Cor média de um ícone (pixels visíveis, puxando para as mais saturadas).
pub fn icon_color(icon: &Pixmap) -> Option<[u8; 3]> {
    let (mut r, mut g, mut b, mut wsum) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for p in icon.pixels().iter().step_by(3) {
        let a = p.alpha();
        if a < 128 {
            continue;
        }
        let c = p.demultiply();
        let (pr, pg, pb) = (c.red() as f32, c.green() as f32, c.blue() as f32);
        let sat = pr.max(pg).max(pb) - pr.min(pg).min(pb);
        let w = 1.0 + sat / 16.0;
        r += pr * w;
        g += pg * w;
        b += pb * w;
        wsum += w;
    }
    (wsum > 0.0).then(|| [(r / wsum) as u8, (g / wsum) as u8, (b / wsum) as u8])
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
    fast: bool,
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

    // Cor dinâmica: a do ícone do app em foco, misturada na borda (e um véu bem leve no fundo).
    let dyn_color = crate::extras::on("fx_dyncolor")
        .then(|| focus.and_then(|i| apps.get(i)).and_then(|a| a.icon.as_ref()).and_then(icon_color))
        .flatten();
    let k = crate::extras::num("fx_dyncolor_level") / 100.0;
    let mix = |c: [u8; 4], to: [u8; 3], f: f32| {
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * f).round() as u8;
        [m(c[0], to[0]), m(c[1], to[1]), m(c[2], to[2]), c[3].max((255.0 * f * 0.7) as u8)]
    };
    let border_color = match dyn_color {
        Some(c) if !theme.minimal => mix(theme.border, c, k),
        _ => theme.border,
    };
    let isl = islands(slots, seps, m, pm.height() as f32);
    let backs: Vec<(f32, f32, f32, f32)> = if isl.is_empty() {
        vec![(l, t, r, b)]
    } else {
        isl.iter().map(|&(a, b_, c, d)| (a as f32, b_ as f32, c as f32, d as f32)).collect()
    };
    for &(l, t, r, b) in &backs {
    // Vidro: o desfoque vem da janela de fundo; aqui só a tinta e a borda.
    if let Some(bg) = rounded_rect(l, t, r - l, b - t, m.radius) {
        pm.fill_path(&bg, &solid(theme.tint), FillRule::Winding, id, None);
        let [sr, sg, sb, sa] = theme.sheen;
        let c = |a: u8| Color::from_rgba8(sr, sg, sb, a);
        let stops = vec![
            GradientStop::new(0.0, c(sa)),
            GradientStop::new(0.4, c(0)),
            GradientStop::new(0.65, c(0)),
            GradientStop::new(1.0, c(sa)),
        ];
        if let Some(shader) =
            LinearGradient::new(Point::from_xy(0.0, t), Point::from_xy(0.0, b), stops, SpreadMode::Pad, id)
        {
            let paint = Paint { shader, anti_alias: true, ..Default::default() };
            pm.fill_path(&bg, &paint, FillRule::Winding, id, None);
        }
    }
    if let Some(border) = rounded_rect(l + 0.5, t + 0.5, r - l - 1.0, b - t - 1.0, (m.radius - 0.5).max(0.0)) {
        let stroke = Stroke { width: 1.0, ..Default::default() };
        pm.stroke_path(&border, &solid(border_color), &stroke, id, None);
    }
    if let (Some(c), false) = (dyn_color, theme.minimal)
        && let Some(bg) = rounded_rect(l, t, r - l, b - t, m.radius)
    {
        pm.fill_path(&bg, &solid([c[0], c[1], c[2], (k * 40.0) as u8]), FillRule::Winding, id, None);
    }
    }

    // Divisores: 1 px, quase a altura do ícone, centralizados no fundo.
    let div_h = (m.base * 0.9).round();
    let div_top = ((t + b - div_h) / 2.0).round();
    for s in seps.iter().filter(|_| isl.is_empty()).filter_map(|&s| slots.get(s)) {
        let x = (s.x + s.size + (m.gap + m.sep) / 2.0).round() + 0.5;
        if let Some(rect) = Rect::from_xywh(x - 0.5, div_top, 1.0, div_h) {
            pm.fill_rect(rect, &solid(theme.divider), id, None);
        }
    }

    // Bilinear nos quadros intermediários da animação (~2,5x mais barato); o quadro final sai em Bicubic.
    let quality = if fast { FilterQuality::Bilinear } else { FilterQuality::Bicubic };
    let icon_paint = PixmapPaint { quality, ..Default::default() };
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
        // Progresso do app (download, cópia, exportação): barrinha discreta no pé do ícone.
        if let Some((state, v)) = crate::progress::of(&app.windows) {
            let bw = slot.size * 0.72;
            let bh = (slot.size * 0.075).max(3.0);
            let bx = sx + (slot.size - bw) / 2.0;
            let by = y + slot.size - bh - slot.size * 0.03;
            if let Some(track) = rounded_rect(bx - 1.0, by - 1.0, bw + 2.0, bh + 2.0, (bh + 2.0) / 2.0) {
                pm.fill_path(&track, &solid([0, 0, 0, 150]), FillRule::Winding, id, None);
            }
            let color = match state {
                crate::progress::ERROR => [248, 113, 113, 255],
                crate::progress::PAUSED => [250, 204, 21, 255],
                _ => [theme.accent[0], theme.accent[1], theme.accent[2], 255],
            };
            let (fx, fw) = if state == crate::progress::INDETERMINATE { (bx + bw * 0.3, bw * 0.4) } else { (bx, bw * v.max(0.04)) };
            if let Some(fill) = rounded_rect(fx, by, fw, bh, bh / 2.0) {
                pm.fill_path(&fill, &solid(color), FillRule::Winding, id, None);
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
            // Luz sob o app em foco: anéis cada vez maiores e mais fracos, na cor do indicador.
            if focus == Some(i) && crate::extras::on("fx_glow") {
                let level = crate::extras::num("fx_glow_level") / 100.0;
                let [cr, cg, cb, _] = color;
                let steps = 6;
                for k in (1..=steps).rev() {
                    let e = m.dot * 1.4 * k as f32;
                    let a = (level * 70.0 / (k as f32 + 1.0)).round() as u8;
                    if let Some(p) = rounded_rect(x - e, y - e * 0.6, w + 2.0 * e, h + 1.2 * e, (h + 1.2 * e) / 2.0) {
                        pm.fill_path(&p, &solid([cr, cg, cb, a]), FillRule::Winding, id, None);
                    }
                }
            }
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
