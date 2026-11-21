//! Lightweight, embedded localization. The app follows Windows' current user UI language.

use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use windows::Win32::Globalization::GetUserDefaultUILanguage;

static LANGUAGE_ID: AtomicU16 = AtomicU16::new(0);
static USE_TRADITIONAL_CHINESE: AtomicBool = AtomicBool::new(false);

fn is_chinese_ui(language_id: u16) -> bool {
    // Windows LANGID primary-language bits. ArcDockWin currently ships a Traditional Chinese
    // pack, so Chinese UI languages use zh-TW wording and other languages keep the source text.
    language_id & 0x03ff == 0x0004
}

fn current_language_id() -> u16 {
    unsafe { GetUserDefaultUILanguage() }
}

/// Read the UI language before creating any windows.
pub fn initialize() {
    let id = current_language_id();
    LANGUAGE_ID.store(id, Ordering::Relaxed);
    USE_TRADITIONAL_CHINESE.store(is_chinese_ui(id), Ordering::Relaxed);
}

/// Poll Windows' current user UI language. Returns true when the active language changed.
pub fn refresh_language() -> bool {
    let id = current_language_id();
    let old = LANGUAGE_ID.swap(id, Ordering::AcqRel);
    if old == id {
        return false;
    }
    USE_TRADITIONAL_CHINESE.store(is_chinese_ui(id), Ordering::Release);
    true
}

pub fn is_traditional_chinese() -> bool {
    USE_TRADITIONAL_CHINESE.load(Ordering::Acquire)
}

/// Locale used by DirectWrite for shaping and text metrics.
pub fn text_locale() -> &'static str {
    if is_traditional_chinese() {
        "zh-TW"
    } else {
        "pt-BR"
    }
}

/// Translate a UI phrase using the embedded `locales/zh-TW.tsv` language pack.
pub fn tr(source: &str) -> String {
    if !is_traditional_chinese() {
        return source.to_owned();
    }
    if let Some((_, translated)) = include_str!("../locales/zh-TW.tsv")
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .find(|(key, _)| *key == source)
    {
        return translated.to_owned();
    }

    // A few labels include a live count/value and therefore cannot be exact table keys.
    if let Some(value) = source.strip_prefix("Bateria ")
        && value.ends_with('%')
    {
        return format!("電池 {value}");
    }
    if let Some(count) = source.strip_suffix(" resultado") {
        return format!("{count} 個結果");
    }
    if let Some(count) = source.strip_suffix(" resultados") {
        return format!("{count} 個結果");
    }
    if let Some(count) = source
        .strip_prefix("Fechar ")
        .and_then(|s| s.strip_suffix(" janelas"))
    {
        return format!("關閉 {count} 個視窗");
    }
    if let Some(count) = source
        .strip_prefix("A cada ")
        .and_then(|s| s.strip_suffix(" horas"))
    {
        return format!("每 {count} 小時");
    }
    source.to_owned()
}

#[cfg(test)]
mod tests {
    use super::is_chinese_ui;

    #[test]
    fn recognizes_windows_chinese_ui_language_ids() {
        assert!(is_chinese_ui(0x0404)); // zh-TW
        assert!(is_chinese_ui(0x0804)); // zh-CN (shown in Traditional, the bundled pack)
        assert!(is_chinese_ui(0x0c04)); // zh-HK
        assert!(!is_chinese_ui(0x0409)); // en-US
        assert!(!is_chinese_ui(0x0416)); // pt-BR
    }
}
