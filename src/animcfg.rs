//! Configuração da animação de minimizar (tela de Configurações). Arquivo
//! `%APPDATA%\ArcDockWin\animation.txt` com linhas `chave=valor`:
//! `enabled=1|0` e `effect=genie|scale`. Padrão: ligada, Gênio.

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Genie,
    Scale,
}

impl Effect {
    pub const ALL: [Effect; 2] = [Effect::Genie, Effect::Scale];

    /// Rótulo em pt-BR (passa por `i18n::tr` na interface).
    pub fn label(self) -> &'static str {
        match self {
            Effect::Genie => "Gênio",
            Effect::Scale => "Escala",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Effect::Genie => "genie",
            Effect::Scale => "scale",
        }
    }
}

// Cache: bit 7 = carregado, bit 0 = ligado, bit 1 = escala.
static STATE: AtomicU8 = AtomicU8::new(0);

fn path() -> std::path::PathBuf {
    crate::apps::config_path().with_file_name("animation.txt")
}

fn load() -> u8 {
    let s = STATE.load(Ordering::Acquire);
    if s & 0x80 != 0 {
        return s;
    }
    let (mut enabled, mut scale) = (true, false);
    if let Ok(text) = std::fs::read_to_string(path()) {
        for line in text.lines() {
            match line.trim().split_once('=') {
                Some(("enabled", v)) => enabled = v.trim() != "0",
                Some(("effect", v)) => scale = v.trim() == "scale",
                _ => {}
            }
        }
    }
    let s = 0x80 | enabled as u8 | (scale as u8) << 1;
    STATE.store(s, Ordering::Release);
    s
}

fn save(enabled: bool, effect: Effect) {
    STATE.store(0x80 | enabled as u8 | ((effect == Effect::Scale) as u8) << 1, Ordering::Release);
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(p, format!("enabled={}\neffect={}\n", enabled as u8, effect.name()));
}

pub fn enabled() -> bool {
    load() & 1 != 0
}

pub fn effect() -> Effect {
    if load() & 2 != 0 { Effect::Scale } else { Effect::Genie }
}

pub fn set_enabled(on: bool) {
    save(on, effect());
}

pub fn set_effect(effect: Effect) {
    save(enabled(), effect);
}
