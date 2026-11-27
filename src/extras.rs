//! Funções extras do dock (efeitos, atalhos, ferramentas, zonas de janela e pré-visualização).
//! Todas começam desligadas. Cada opção é uma linha desta tabela: a janela de Configurações
//! monta as páginas a partir dela e grava em `settings.txt` (mesmo arquivo das outras opções).

use crate::config;

#[derive(Clone, Copy)]
pub enum Kind {
    /// Liga/desliga (padrão desligado).
    Toggle,
    /// Controle deslizante em números inteiros: mínimo, máximo, padrão e unidade.
    Slider { min: f32, max: f32, def: f32, unit: &'static str },
    /// Escolha entre opções (segmentos); padrão = 0.
    Choice(&'static [&'static str]),
}

pub struct Extra {
    pub key: &'static str,
    pub label: &'static str,
    /// Explicação embaixo do texto (vazio = nenhuma).
    pub sub: &'static str,
    pub kind: Kind,
    /// Só vale (e só se mexe) com esta outra opção ligada; vazio = sempre.
    pub needs: &'static str,
}

const fn t(key: &'static str, label: &'static str, sub: &'static str) -> Extra {
    Extra { key, label, sub, kind: Kind::Toggle, needs: "" }
}

const fn s(key: &'static str, label: &'static str, min: f32, max: f32, def: f32, unit: &'static str, needs: &'static str) -> Extra {
    Extra { key, label, sub: "", kind: Kind::Slider { min, max, def, unit }, needs }
}

pub const STYLES: &[&str] = &["Sistema", "Escuro", "Claro", "Mínimo"];

pub const EXTRAS: &[Extra] = &[
    // Efeitos
    Extra { key: "fx_style", label: "Estilo do fundo", sub: "", kind: Kind::Choice(STYLES), needs: "" },
    t("fx_glow", "Luz sob o app em foco", "Um brilho suave no indicador"),
    s("fx_glow_level", "Intensidade da luz", 10.0, 100.0, 40.0, "%", "fx_glow"),
    t("fx_dyncolor", "Cor dinâmica na borda", "Usa a cor do ícone do app em foco"),
    s("fx_dyncolor_level", "Intensidade da cor", 10.0, 100.0, 35.0, "%", "fx_dyncolor"),
    t("fx_islands", "Dock em ilhas", "Separa os grupos onde há separadores"),
    s("fx_islands_gap", "Espaço entre as ilhas", 4.0, 40.0, 12.0, "px", "fx_islands"),
    s("fx_dot_level", "Opacidade dos indicadores", 20.0, 100.0, 100.0, "%", ""),
    t("fx_progress", "Progresso nos ícones", "A barrinha de download, cópia ou exportação dos apps"),
    // Atalhos
    t("pin_heal", "Corrigir atalhos quando um app atualiza", "Acha a versão nova na mesma pasta"),
    t("pin_drop", "Soltar arquivos no dock", "Sobre um app: abre com ele. Fora dos ícones: fixa"),
    t("pin_folder_stack", "Pastas fixadas abrem como pilha", "Mostra o conteúdo acima do ícone"),
    t("pin_profiles", "Separar perfis do mesmo app", "Ex.: dois perfis do Claude, cada um no seu ícone"),
    // Mouse
    t("cur_shake", "Sacudir o mouse aumenta o cursor", "Feche o Shake to Find Cursor antes, para não brigarem"),
    s("cur_shake_size", "Tamanho ao sacudir", 150.0, 500.0, 300.0, "%", "cur_shake"),
    s("cur_shake_sens", "Sensibilidade", 1.0, 10.0, 5.0, "", "cur_shake"),
    // Ferramentas
    t("tool_shelf", "Prateleira", "Guarda arquivos de passagem (só o caminho)"),
    s("shelf_limit", "Itens na prateleira", 5.0, 50.0, 20.0, "", "tool_shelf"),
    t("tool_clip", "Histórico da área de transferência", "Só texto, na memória; nada vai para o disco"),
    s("clip_limit", "Itens no histórico", 10.0, 100.0, 30.0, "", "tool_clip"),
    t("tool_picker", "Coletor de cores", "Clique na ferramenta e depois em qualquer ponto da tela"),
    Extra { key: "picker_format", label: "Formato copiado", sub: "", kind: Kind::Choice(&["HEX", "RGB"]), needs: "tool_picker" },
    t("tool_timer", "Timer de foco", "Mostra o tempo restante no ícone"),
    s("timer_min", "Duração", 5.0, 90.0, 25.0, "min", "tool_timer"),
    t("tool_music", "Música ao lado do dock", "Capa, controles e a barra da faixa num quadro à parte"),
    // Janelas
    t("zones_on", "Zona ao arrastar com Shift", "Desligue o FancyZones antes, para não brigarem"),
    s("zone_left", "Margem esquerda", 0.0, 300.0, 31.0, "px", "zones_on"),
    s("zone_top", "Margem de cima", 0.0, 300.0, 1.0, "px", "zones_on"),
    s("zone_right", "Margem direita", 0.0, 300.0, 25.0, "px", "zones_on"),
    s("zone_bottom", "Margem de baixo", 0.0, 300.0, 32.0, "px", "zones_on"),
    s("zone_flash", "Destaque da zona", 5.0, 60.0, 18.0, "%", "zones_on"),
    t("shadow_on", "Sombra extra na janela encaixada", "Desenhada pelo dock, atrás da janela"),
    s("shadow_level", "Intensidade da sombra", 5.0, 100.0, 35.0, "%", "shadow_on"),
    s("shadow_soft", "Suavidade da sombra", 4.0, 60.0, 22.0, "px", "shadow_on"),
    // Pré-visualização
    t("ql_on", "Pré-visualizar com Espaço", "No Explorador; pausa enquanto o Peek estiver aberto"),
    t("ql_ctrl", "Usar Ctrl+Espaço", "Em vez de só Espaço"),
    s("ql_size", "Tamanho da janela", 40.0, 95.0, 72.0, "%", "ql_on"),
    s("ql_text_kb", "Limite para texto", 64.0, 4096.0, 1024.0, "KB", "ql_on"),
];

/// Páginas extras das Configurações: título, glifo e cartões (chaves de cada cartão).
pub const PAGES: &[(&str, u16, &[&[&str]])] = &[
    (
        "Efeitos",
        0xE706,
        &[
            &["fx_style", "fx_dot_level", "fx_progress"],
            &["fx_glow", "fx_glow_level", "fx_dyncolor", "fx_dyncolor_level"],
            &["fx_islands", "fx_islands_gap"],
        ],
    ),
    (
        "Atalhos e mouse",
        0xE71B,
        &[&["pin_heal", "pin_drop", "pin_folder_stack", "pin_profiles"], &["cur_shake", "cur_shake_size", "cur_shake_sens"]],
    ),
    (
        "Ferramentas",
        0xE90F,
        &[
            &["tool_shelf", "shelf_limit", "tool_clip", "clip_limit"],
            &["tool_picker", "picker_format", "tool_timer", "timer_min", "tool_music"],
        ],
    ),
    (
        "Janelas",
        0xE7C4,
        &[
            &["zones_on", "zone_left", "zone_top", "zone_right", "zone_bottom", "zone_flash"],
            &["shadow_on", "shadow_level", "shadow_soft"],
        ],
    ),
    ("Pré-visualização", 0xE7B3, &[&["ql_on", "ql_ctrl", "ql_size", "ql_text_kb"]]),
];

pub fn index(key: &str) -> Option<usize> {
    EXTRAS.iter().position(|e| e.key == key)
}

/// Opção ligada (as que não são liga/desliga contam como ligadas).
pub fn on(key: &str) -> bool {
    match index(key).map(|i| EXTRAS[i].kind) {
        Some(Kind::Toggle) | None => config::raw(key).is_some_and(|v| v == "1"),
        _ => true,
    }
}

/// Valor de um controle deslizante (ou da escolha, como número).
pub fn num(key: &str) -> f32 {
    match index(key).map(|i| EXTRAS[i].kind) {
        Some(Kind::Slider { min, max, def, .. }) => {
            config::raw(key).and_then(|v| v.parse().ok()).unwrap_or(def).clamp(min, max).round()
        }
        Some(Kind::Choice(opts)) => {
            config::raw(key).and_then(|v| v.parse::<usize>().ok()).filter(|&v| v < opts.len()).unwrap_or(0) as f32
        }
        _ => on(key) as u8 as f32,
    }
}

pub fn choice(key: &str) -> usize {
    num(key) as usize
}

/// Habilitada: a opção de que ela depende está ligada.
pub fn enabled(i: usize) -> bool {
    let n = EXTRAS[i].needs;
    n.is_empty() || on(n)
}

/// Fração (0..1) do controle deslizante `i`.
pub fn fraction(i: usize) -> f32 {
    match EXTRAS[i].kind {
        Kind::Slider { min, max, .. } => (num(EXTRAS[i].key) - min) / (max - min),
        _ => 0.0,
    }
}

pub fn step(i: usize) -> f32 {
    match EXTRAS[i].kind {
        Kind::Slider { min, max, .. } => 1.0 / (max - min),
        _ => 0.1,
    }
}

pub fn label(i: usize) -> String {
    match EXTRAS[i].kind {
        Kind::Slider { unit, .. } => {
            let v = num(EXTRAS[i].key);
            if unit.is_empty() { format!("{v}") } else if unit == "%" { format!("{v}%") } else { format!("{v} {unit}") }
        }
        _ => String::new(),
    }
}

/// Muda o controle deslizante `i` para a fração `v`; devolve se mudou.
pub fn slide(i: usize, v: f32) -> bool {
    let Kind::Slider { min, max, .. } = EXTRAS[i].kind else { return false };
    let x = (min + v.clamp(0.0, 1.0) * (max - min)).round();
    let key = EXTRAS[i].key;
    if x == num(key) {
        return false;
    }
    config::set_raw(key, x.to_string());
    changed(key);
    true
}

pub fn toggle(i: usize) {
    let key = EXTRAS[i].key;
    config::set_raw(key, if on(key) { "0".into() } else { "1".into() });
    changed(key);
}

pub fn set_choice(i: usize, c: usize) {
    let key = EXTRAS[i].key;
    config::set_raw(key, c.to_string());
    changed(key);
}

/// Aplica a mudança da opção `key` em quem depende dela.
pub fn changed(key: &str) {
    match key.split('_').next().unwrap_or("") {
        "fx" => {
            crate::progress::sync();
            crate::apply_settings(false);
        }
        "cur" => crate::shake::sync(),
        "tool" | "shelf" | "clip" | "picker" | "timer" => {
            crate::clipboard::sync();
            crate::tools::sync();
            crate::apply_settings(false);
            crate::musicbar::sync();
        }
        "zones" | "zone" | "shadow" => crate::zones::sync(),
        "ql" => crate::quicklook::sync(),
        "pin" => {
            crate::reload_pins();
            crate::apply_settings(false);
        }
        _ => {}
    }
}
