//! Atalhos que quebram quando um app se atualiza: muitos apps guardam o .exe numa pasta com a
//! versão no nome (`Discord\app-1.0.9270\Discord.exe`, `WindowsApps\App_2.1.0.0_x64__id\...`).
//! Quando o caminho fixado some, procura a pasta irmã da mesma família com a versão mais nova
//! que tenha o mesmo resto do caminho. Nos apps da Store a pasta WindowsApps não se lista sem
//! administrador, então a lista de pacotes vem do registro do usuário.

use std::path::{Path, PathBuf};

/// Família de uma pasta com versão: o nome sem dígitos nem pontos (`app-1.0.9` → `app-`).
fn family(name: &str) -> Option<String> {
    if !name.chars().any(|c| c.is_ascii_digit()) || !name.contains('.') {
        return None;
    }
    Some(name.chars().filter(|c| !c.is_ascii_digit() && *c != '.').collect::<String>().to_lowercase())
}

/// Números da versão no nome, para comparar (`1.0.10` > `1.0.9`).
fn version(name: &str) -> Vec<u64> {
    name.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).filter_map(|p| p.parse().ok()).collect()
}

/// Caminho novo para `old`, se houver um só jeito óbvio de achá-lo.
pub fn find(old: &str) -> Option<String> {
    let path = Path::new(old);
    let comps: Vec<String> = path.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    // Da pasta mais funda para cima (o nome do arquivo não conta).
    for i in (1..comps.len().saturating_sub(1)).rev() {
        let Some(fam) = family(&comps[i]) else { continue };
        let parent: PathBuf = comps[..i].iter().collect();
        let rest: PathBuf = comps[i + 1..].iter().collect();
        let names = siblings(&parent, &comps[i]);
        let best = names
            .into_iter()
            .filter(|n| family(n).as_deref() == Some(fam.as_str()))
            .filter(|n| parent.join(n).join(&rest).exists())
            .max_by(|a, b| version(a).cmp(&version(b)))?;
        return Some(parent.join(best).join(rest).to_string_lossy().into_owned());
    }
    None
}

/// Pastas em `parent`. Na WindowsApps (sem permissão de listar), os pacotes do usuário com o
/// mesmo nome e editor que `name`.
fn siblings(parent: &Path, name: &str) -> Vec<String> {
    if let Ok(rd) = std::fs::read_dir(parent) {
        return rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    }
    packages(name)
}

#[cfg(windows)]
fn packages(name: &str) -> Vec<String> {
    use windows::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegEnumKeyW, RegOpenKeyExW};
    use windows::core::HSTRING;
    // Nome do pacote: Nome_Versão_Arquitetura__Editor.
    let (Some(pkg), Some(publisher)) = (name.split('_').next(), name.rsplit("__").next()) else { return Vec::new() };
    let key = r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages";
    let mut out = Vec::new();
    unsafe {
        let mut h = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(key), None, KEY_READ, &mut h).is_err() {
            return out;
        }
        let mut buf = [0u16; 512];
        let mut i = 0;
        while RegEnumKeyW(h, i, Some(&mut buf)).is_ok() {
            let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let sub = String::from_utf16_lossy(&buf[..n]);
            if sub.split('_').next() == Some(pkg) && sub.ends_with(&format!("__{publisher}")) {
                out.push(sub);
            }
            i += 1;
        }
        let _ = RegCloseKey(h);
    }
    out
}

#[cfg(not(windows))]
fn packages(_: &str) -> Vec<String> {
    Vec::new()
}

/// Grava os caminhos corrigidos no pinned.txt (guarda uma cópia antes).
pub fn rewrite(cfg: &Path, healed: &[(String, String)]) {
    let Ok(text) = std::fs::read_to_string(cfg) else { return };
    let _ = std::fs::write(cfg.with_extension("txt.antes-da-correcao"), &text);
    let out: Vec<String> = text
        .lines()
        .map(|l| {
            let src = crate::apps::line_source(l);
            match healed.iter().find(|(old, _)| old == src) {
                Some((old, new)) => l.replacen(old.as_str(), new, 1),
                None => l.to_string(),
            }
        })
        .collect();
    let _ = std::fs::write(cfg, out.join("\n") + "\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn familia_e_versao() {
        assert_eq!(family("app-1.0.9270"), family("app-1.0.10012"));
        assert_eq!(family("Claude_6.28899.6.0_x64__pzs8sxrjxfjjc"), family("Claude_7.1.0.0_x64__pzs8sxrjxfjjc"));
        assert_ne!(family("Claude_6.1.0.0_x64__abc"), family("Other_6.1.0.0_x64__abc"));
        assert_eq!(family("Programs"), None);
        assert!(version("app-1.0.10012") > version("app-1.0.9270"));
    }

    #[test]
    fn acha_pasta_nova() {
        let base = std::env::temp_dir().join(format!("heal-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for v in ["app-1.0.9", "app-1.0.12", "app-1.0.11"] {
            std::fs::create_dir_all(base.join(v)).unwrap();
            std::fs::write(base.join(v).join("x.exe"), "").unwrap();
        }
        std::fs::create_dir_all(base.join("app-9.9.9")).unwrap(); // sem o exe: não serve
        let old = base.join("app-1.0.1").join("x.exe");
        let got = find(&old.to_string_lossy()).unwrap();
        assert!(got.ends_with(&*Path::new("app-1.0.12").join("x.exe").to_string_lossy()), "{got}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
