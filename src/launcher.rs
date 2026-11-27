//! Lançador (tecla Windows ou botão Iniciar), no jeito do Spotlight: abre com a lista de apps
//! (os mais abertos primeiro) e, digitando, mostra o melhor resultado em destaque e o resto agrupado: aplicativos,
//! configurações do Windows, ações (bloquear, desligar...) e documentos recentes. Também faz
//! contas e conversões de unidade (Enter copia o resultado).
//!
//! Apps vêm da pasta Aplicativos do Windows (`shell:AppsFolder`, com os apps da Store e os
//! atalhos do menu Iniciar); documentos, da pasta de itens recentes. O que é mais aberto sobe
//! na lista (contagem em `%APPDATA%\ArcDockWin\launches.txt`).

use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use tiny_skia::Pixmap;
use windows::Win32::Foundation::{GlobalFree, HANDLE, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VIRTUAL_KEY, VK_CONTROL, VK_DOWN, VK_SHIFT, VK_UP};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, KF_FLAG_DEFAULT, SHGetKnownFolderItem,
    SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, PostMessageW, SW_SHOWNORMAL, WM_APP};
use windows::core::{HSTRING, w};

use crate::apps::{APPS_FOLDER, load_icon};
use crate::i18n::tr;
use crate::panel::{self, Event, Item, PanelContent, Widget};
use crate::power;

const FIELD: u32 = 1;
/// Botão "Admin" da linha selecionada (abre o app como administrador).
const RUN_AS_ADMIN: u32 = 2;
/// Ids das linhas: ROW + posição entre todos os resultados.
const ROW: u32 = 100;
/// Linhas visíveis da lista sem busca; o resto rola com as setas ou a roda do mouse.
const VISIBLE: usize = 8;
/// Apps com ícone já carregado na pré-carga (a primeira tela).
const FIRST_ICONS: usize = VISIBLE;
/// Máximo de linhas por grupo.
const MAX_APPS: usize = 5;
const MAX_SETTINGS: usize = 3;
const MAX_ACTIONS: usize = 2;
const MAX_DOCS: usize = 3;
/// Documentos recentes guardados para a busca.
const MAX_RECENT: usize = 400;
const CF_UNICODETEXT: u32 = 13;

struct Entry {
    name: String,
    /// Nome em minúsculas, sem acentos (para a busca).
    key: String,
    /// Nome de análise dentro da pasta Aplicativos (AUMID ou caminho).
    id: String,
    /// App do dia a dia (veja [`useful`]): os outros ficam atrás na busca.
    useful: bool,
}

/// Documento recente: atalho da pasta Recent do Windows.
struct Doc {
    name: String,
    key: String,
    path: String,
}

/// Páginas das Configurações do Windows: (nome, outras palavras sem acento, endereço).
const SETTINGS: [(&str, &str, &str); 26] = [
    ("Wi-Fi", "wifi rede internet wireless sem fio", "ms-settings:network-wifi"),
    ("Bluetooth e dispositivos", "bluetooth fone mouse teclado dispositivos", "ms-settings:bluetooth"),
    ("Som", "som audio volume microfone alto-falante", "ms-settings:sound"),
    ("Tela", "tela monitor resolucao brilho escala display", "ms-settings:display"),
    ("Plano de fundo", "papel de parede wallpaper fundo personalizacao", "ms-settings:personalization-background"),
    ("Cores", "cores tema escuro claro modo destaque", "ms-settings:colors"),
    ("Notificações", "notificacoes avisos alertas", "ms-settings:notifications"),
    ("Não incomodar", "nao incomodar foco silencio", "ms-settings:quiethours"),
    ("Energia e bateria", "energia bateria suspensao sono", "ms-settings:powersleep"),
    ("Armazenamento", "armazenamento disco espaco limpeza", "ms-settings:storagesense"),
    ("Aplicativos instalados", "aplicativos programas desinstalar remover", "ms-settings:appsfeatures"),
    ("Aplicativos padrão", "padrao navegador abrir com", "ms-settings:defaultapps"),
    ("Aplicativos de inicialização", "inicializacao iniciar com o windows startup", "ms-settings:startupapps"),
    ("Suas informações", "conta perfil usuario foto", "ms-settings:yourinfo"),
    ("Data e hora", "data hora relogio fuso horario", "ms-settings:dateandtime"),
    ("Idioma e região", "idioma regiao lingua pais formato", "ms-settings:regionlanguage"),
    ("Teclado", "teclado digitacao layout", "ms-settings:keyboard"),
    ("Mouse", "mouse ponteiro cursor touchpad", "ms-settings:mousetouchpad"),
    ("Impressoras e scanners", "impressora scanner imprimir", "ms-settings:printers"),
    ("Windows Update", "atualizacao update atualizar", "ms-settings:windowsupdate"),
    ("Privacidade e segurança", "privacidade seguranca permissoes camera localizacao", "ms-settings:privacy"),
    ("VPN", "vpn rede", "ms-settings:network-vpn"),
    ("Rede e internet", "rede internet ethernet conexao", "ms-settings:network-status"),
    ("Acessibilidade", "acessibilidade lupa narrador contraste", "ms-settings:easeofaccess"),
    ("Sobre", "sobre sistema versao pc nome do computador especificacoes", "ms-settings:about"),
    ("Configurações", "configuracoes ajustes painel de controle opcoes", "ms-settings:"),
];

/// Ações do sistema: (ação de [`power`], glifo, nome, outras palavras).
const ACTIONS: [(u32, u16, &str, &str); 5] = [
    (power::LOCK, 0xE72E, "Bloquear", "bloquear travar lock"),
    (power::SLEEP, 0xE708, "Suspender", "suspender dormir sleep"),
    (power::SIGN_OUT, 0xF3B1, "Sair", "sair logoff encerrar sessao"),
    (power::RESTART, 0xE777, "Reiniciar", "reiniciar restart reboot"),
    (power::SHUTDOWN, 0xE7E8, "Desligar", "desligar shutdown"),
];

/// Apps, documentos e ícones já carregados, para o lançador abrir na hora (montar a lista de
/// apps leva ~200 ms).
static APPS: Mutex<Option<Arc<Vec<Entry>>>> = Mutex::new(None);
static DOCS: Mutex<Option<Arc<Vec<Doc>>>> = Mutex::new(None);
static ICONS: LazyLock<Mutex<HashMap<String, Option<Pixmap>>>> = LazyLock::new(Default::default);
static LOADING: AtomicBool = AtomicBool::new(false);
/// Quando a última pré-carga começou: abrir o lançador várias vezes seguidas não relê tudo.
static LAST_LOAD: Mutex<Option<Instant>> = Mutex::new(None);
/// Ícones pedidos e ainda não carregados (chaves de [`ICONS`]), e se a thread que os carrega
/// está rodando.
static WANTED: LazyLock<Mutex<(Vec<(String, i32)>, std::collections::HashSet<String>, bool)>> =
    LazyLock::new(Default::default);
/// Já há um [`WM_ICONS`] a caminho (vários ícones prontos viram um redesenho só).
static ICONS_POSTED: AtomicBool = AtomicBool::new(false);
/// Postada à topbar quando ícones novos ficaram prontos; ela repassa ao painel.
pub const WM_ICONS: u32 = WM_APP + 52;
/// Quantas vezes cada coisa foi aberta pelo lançador.
static LAUNCHES: LazyLock<Mutex<HashMap<String, u32>>> = LazyLock::new(|| Mutex::new(load_launches()));

fn launches_path() -> std::path::PathBuf {
    crate::apps::config_path().with_file_name("launches.txt")
}

fn load_launches() -> HashMap<String, u32> {
    std::fs::read_to_string(launches_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (n, key) = l.split_once('\t')?;
            Some((key.to_string(), n.parse().ok()?))
        })
        .collect()
}

fn launches(key: &str) -> u32 {
    LAUNCHES.lock().ok().and_then(|m| m.get(key).copied()).unwrap_or(0)
}

fn count_launch(key: String) {
    let Ok(mut m) = LAUNCHES.lock() else { return };
    *m.entry(key).or_insert(0) += 1;
    let text: String = m.iter().map(|(k, n)| format!("{n}\t{k}\n")).collect();
    let _ = std::fs::write(launches_path(), text);
}

/// Carrega (ou recarrega) apps, documentos recentes e os primeiros ícones em segundo plano.
pub fn preload() {
    {
        let Ok(mut last) = LAST_LOAD.lock() else { return };
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(30)) {
            return;
        }
        if LOADING.swap(true, Ordering::AcqRel) {
            return;
        }
        *last = Some(Instant::now());
    }
    std::thread::spawn(|| {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let all = installed();
        let docs = recent_docs();
        // Os mais abertos primeiro: são os que aparecem como melhor resultado.
        let mut first: Vec<&Entry> = all.iter().filter(|e| e.useful).collect();
        first.sort_by_key(|e| Reverse(launches(&app_key(e))));
        for e in first.into_iter().take(FIRST_ICONS) {
            cached_icon(&app_source(e), icon_px(false));
            cached_icon(&app_source(e), icon_px(true));
        }
        if let Ok(mut apps) = APPS.lock() {
            *apps = Some(Arc::new(all));
        }
        if let Ok(mut d) = DOCS.lock() {
            *d = Some(Arc::new(docs));
        }
        LOADING.store(false, Ordering::Release);
    });
}

fn icon_px(big: bool) -> i32 {
    let dip = if big { 32.0 } else { 24.0 };
    (dip * unsafe { GetDpiForSystem() } as f32 / 96.0).round() as i32
}

fn app_source(e: &Entry) -> String {
    format!("{APPS_FOLDER}{}", e.id)
}

fn app_key(e: &Entry) -> String {
    format!("app:{}", e.id)
}

/// Ícone de `source` (item da pasta Aplicativos ou arquivo), carregado uma vez e guardado.
fn cached_icon(source: &str, px: i32) -> Option<Pixmap> {
    let key = format!("{px}|{source}");
    if let Some(icon) = ICONS.lock().ok()?.get(&key) {
        return icon.clone();
    }
    // Carrega fora do lock: a outra thread (pré-carga ou lançador) não espera.
    let icon = load_icon(source, px);
    ICONS.lock().ok()?.insert(key, icon.clone());
    icon
}

/// Ícone já carregado de `source`; se ainda não estiver, pede para outra thread carregar
/// (a lista aparece na hora com o glifo, e o ícone entra quando chega [`WM_ICONS`]).
fn icon_or_request(source: &str, px: i32) -> Option<Pixmap> {
    let key = format!("{px}|{source}");
    if let Some(icon) = ICONS.lock().ok()?.get(&key) {
        return icon.clone();
    }
    let Ok(mut w) = WANTED.lock() else { return None };
    if w.1.insert(key) {
        w.0.push((source.to_string(), px));
    }
    if !w.2 {
        w.2 = true;
        std::thread::spawn(load_wanted);
    }
    None
}

/// A topbar recebeu [`WM_ICONS`]: os próximos ícones prontos podem postar de novo.
pub fn icons_arrived() {
    ICONS_POSTED.store(false, Ordering::Release);
}

/// Carrega os ícones pedidos em [`icon_or_request`], avisando a topbar a cada um.
fn load_wanted() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    loop {
        let next = {
            let Ok(mut w) = WANTED.lock() else { return };
            // O último pedido primeiro: é o que está na tela agora.
            let next = w.0.pop();
            if next.is_none() {
                w.2 = false;
            }
            next
        };
        let Some((source, px)) = next else { return };
        cached_icon(&source, px);
        if let Ok(mut w) = WANTED.lock() {
            w.1.remove(&format!("{px}|{source}"));
        }
        if !ICONS_POSTED.swap(true, Ordering::AcqRel) {
            unsafe {
                let _ = PostMessageW(Some(crate::topbar::hwnd()), WM_ICONS, WPARAM(0), LPARAM(0));
            }
        }
    }
}

/// Atalhos da pasta de itens recentes do Windows, do mais novo para o mais velho.
fn recent_docs() -> Vec<Doc> {
    let Some(appdata) = std::env::var_os("APPDATA") else { return Vec::new() };
    let dir = std::path::Path::new(&appdata).join(r"Microsoft\Windows\Recent");
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut list: Vec<(std::time::SystemTime, Doc)> = rd
        .flatten()
        .filter_map(|f| {
            let path = f.path();
            if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk")) {
                return None;
            }
            let name = path.file_stem()?.to_string_lossy().into_owned();
            // Links de protocolo que o Windows guarda junto (recorte de tela, Xbox...).
            if name.starts_with("ms-") {
                return None;
            }
            let time = f.metadata().and_then(|m| m.modified()).ok()?;
            Some((time, Doc { key: fold(&name), name, path: path.to_string_lossy().into_owned() }))
        })
        .collect();
    list.sort_by(|a, b| b.0.cmp(&a.0));
    list.into_iter().take(MAX_RECENT).map(|(_, d)| d).collect()
}

/// O que uma linha abre.
#[derive(Clone)]
enum Target {
    App(usize),
    Doc(usize),
    Setting(usize),
    Action(usize),
    /// Conta ou conversão: o texto copiado.
    Calc(String),
    /// Busca nos arquivos pelo Explorador.
    Find,
}

struct Hit {
    target: Target,
    text: String,
    /// 0: nome começa com a busca; 1: uma palavra começa; 2: contém; 3: só palavra-chave.
    tier: u8,
    /// Vezes que foi aberto.
    count: u32,
    /// 1 para apps fora do dia a dia (atrás dos outros no mesmo nível).
    extra: u8,
}

impl Hit {
    fn rank(&self) -> (u8, Reverse<u32>, u8) {
        (self.tier, Reverse(self.count), self.extra)
    }
}

struct Launcher {
    all: Arc<Vec<Entry>>,
    docs: Arc<Vec<Doc>>,
    query: String,
    /// Melhor resultado (linha grande) e o título dele.
    best: Option<Hit>,
    best_label: &'static str,
    groups: Vec<(&'static str, Vec<Hit>)>,
    /// Linha selecionada entre todas (o melhor resultado é a 0).
    sel: usize,
    /// Sem busca: primeira linha visível da lista de apps.
    top: usize,
}

/// Abre (ou fecha, se já aberto) o lançador, centrado no monitor do cursor.
pub fn toggle() {
    let mut pt = POINT::default();
    let work = unsafe {
        let _ = GetCursorPos(&mut pt);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST), &mut mi);
        mi.rcWork
    };
    // O painel abre sob um "item": aqui, uma faixa a 1/5 da altura, com o painel centrado.
    let y = work.top + (work.bottom - work.top) / 5;
    let item = RECT { left: work.left, top: y, right: work.right, bottom: y };
    panel::toggle("launcher", item, || {
        let cached = APPS.lock().ok().and_then(|a| a.clone());
        let all = cached.unwrap_or_else(|| Arc::new(installed()));
        let docs = DOCS.lock().ok().and_then(|d| d.clone()).unwrap_or_default();
        // Atualiza em segundo plano para a próxima vez (app instalado, documento novo).
        preload();
        let mut l = Launcher { all, docs, query: String::new(), best: None, best_label: "", groups: Vec::new(), sel: 0, top: 0 };
        l.filter("");
        Box::new(l)
    });
}

/// Apps da pasta Aplicativos, em ordem alfabética.
fn installed() -> Vec<Entry> {
    let mut out = Vec::new();
    unsafe {
        let Ok(folder) = SHGetKnownFolderItem::<IShellItem>(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None) else {
            return out;
        };
        let Ok(items) = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else { return out };
        let name = |item: &IShellItem, kind| {
            let p = item.GetDisplayName(kind).ok()?;
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as _));
            s
        };
        loop {
            let mut batch = [None];
            let mut n = 0;
            if items.Next(&mut batch, Some(&mut n)).is_err() || n == 0 {
                break;
            }
            let Some(item) = batch[0].take() else { break };
            if let (Some(n), Some(id)) = (name(&item, SIGDN_NORMALDISPLAY), name(&item, SIGDN_PARENTRELATIVEPARSING)) {
                let key = fold(&n);
                out.push(Entry { useful: useful(&key, &id), key, name: n, id });
            }
        }
    }
    // Nomes repetidos (ex.: o app e um atalho gerado para ele): fica o que não é gerado.
    let generated = |e: &Entry| e.id.starts_with("Microsoft.AutoGenerated.");
    out.sort_by(|a, b| a.key.cmp(&b.key).then(generated(a).cmp(&generated(b))));
    out.dedup_by(|b, a| a.key == b.key);
    out
}

/// Pastas do Windows (System32, SysWOW64, Windows) como aparecem nos ids da pasta Aplicativos.
const SYSTEM_DIRS: [&str; 3] = [
    "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}",
    "{D65231B0-B2F1-4857-A4CE-A8E7C6EA7D27}",
    "{F38BF404-1D43-42F2-9305-67DE0B28FC23}",
];

/// Ferramentas das pastas do Windows que ficam na lista mesmo assim.
const SYSTEM_KEEP: [&str; 2] = [r"\cmd.exe", r"\windowspowershell\v1.0\powershell.exe"];

/// Pedaços de nome (sem acento, minúsculos) de coisas que não são apps do dia a dia.
const NOISE: [&str; 37] = [
    "uninstall", "desinstalar", "documentation", "documentacao", "dokumentation", "docs", "manual",
    "readme", "faq", "release notes", "homepage", "website", "sample", "tools for", "native tools",
    "cross tools", "command prompt", "developer powershell", "install additional tools", "installer",
    "install manager", "(x86)", "(wow)", "pydoc", "idle (", "software development kit", "cert kit",
    "debuggable", "application verifier", "window spy", "ise ", "obter ajuda", "introducao",
    "ferramentas do windows", "legacy", "git cmd", "git gui",
];

/// Ferramentas de administração do Windows que vêm como atalhos gerados (sem caminho).
const ADMIN: [&str; 9] = [
    "agendador de tarefas", "console", "gerenciamento do computador", "monitor de desempenho",
    "monitor de recursos", "visualizador de eventos", "executar", "backup do windows", "acao com um clique",
];

/// Se o app aparece na lista sem busca.
fn useful(key: &str, id: &str) -> bool {
    let lid = id.to_lowercase();
    // Links e documentos (sites, páginas de ajuda, .url).
    if lid.starts_with("http") || [".html", ".htm", ".url", ".chm", ".pdf", ".txt"].iter().any(|e| lid.ends_with(e)) {
        return false;
    }
    // Interpretadores soltos (abrem só um console vazio).
    if [r"\python.exe", r"\node.exe"].iter().any(|e| lid.ends_with(e)) {
        return false;
    }
    if NOISE.iter().any(|n| key.contains(n)) || ADMIN.contains(&key) {
        return false;
    }
    // Utilitários das pastas do Windows (msconfig, regedit, serviços, lupa...).
    if SYSTEM_DIRS.iter().any(|d| id.starts_with(d)) {
        return SYSTEM_KEEP.iter().any(|k| lid.ends_with(k));
    }
    true
}

/// Minúsculas e sem acentos, para "música" achar "Musica".
fn fold(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c => c,
        })
        .collect()
}

/// Nível de `key` para a busca `q` (palavras `words`), ou `None` se não bate.
fn tier(key: &str, q: &str, words: &[&str]) -> Option<u8> {
    if !words.iter().all(|w| key.contains(w)) {
        return None;
    }
    Some(if key.starts_with(q) {
        0
    } else if key.split_whitespace().any(|w| w.starts_with(q)) {
        1
    } else {
        2
    })
}

/// Nível pelo nome ou, sem ele, pelas palavras-chave (todas as palavras da busca começam uma).
fn tier_with(name: &str, keywords: &str, q: &str, words: &[&str]) -> Option<u8> {
    tier(&fold(name), q, words)
        .or_else(|| words.iter().all(|w| keywords.split_whitespace().any(|k| k.starts_with(w))).then_some(3))
}

impl Launcher {
    fn filter(&mut self, query: &str) {
        self.query = query.to_string();
        self.sel = 0;
        self.best = None;
        self.top = 0;
        self.groups.clear();
        let q = fold(query.trim());
        if q.is_empty() {
            // Sem busca: os apps do dia a dia, os mais abertos primeiro e o resto em ordem alfabética.
            let mut apps: Vec<Hit> = self
                .all
                .iter()
                .enumerate()
                .filter(|(_, e)| e.useful)
                .map(|(i, e)| Hit { target: Target::App(i), text: e.name.clone(), tier: 0, count: launches(&app_key(e)), extra: 0 })
                .collect();
            apps.sort_by_key(|h| Reverse(h.count)); // estável: mantém a ordem alfabética
            self.groups.push(("Aplicativos", apps));
            return;
        }
        let words: Vec<&str> = q.split_whitespace().collect();
        let len = q.chars().count();
        let hit = |target, text: String, tier, count, extra| Hit { target, text, tier, count, extra };

        let mut apps: Vec<Hit> = self
            .all
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let t = tier(&e.key, &q, &words)?;
                Some(hit(Target::App(i), e.name.clone(), t, launches(&app_key(e)), u8::from(!e.useful)))
            })
            .collect();
        let mut settings: Vec<Hit> = if len < 2 {
            Vec::new()
        } else {
            SETTINGS
                .iter()
                .enumerate()
                .filter_map(|(i, (name, kw, uri))| {
                    let shown = tr(name);
                    let t = tier_with(&shown, kw, &q, &words).or_else(|| tier_with(name, kw, &q, &words))?;
                    Some(hit(Target::Setting(i), shown, t, launches(&format!("set:{uri}")), 0))
                })
                .collect()
        };
        // Ações só com 3 letras: "d" e Enter não podem desligar o PC.
        let mut actions: Vec<Hit> = if len < 3 {
            Vec::new()
        } else {
            ACTIONS
                .iter()
                .enumerate()
                .filter_map(|(i, (_, _, name, kw))| {
                    let shown = tr(name);
                    let t = tier_with(&shown, kw, &q, &words).or_else(|| tier_with(name, kw, &q, &words))?;
                    Some(hit(Target::Action(i), shown, t, launches(&format!("act:{name}")), 0))
                })
                .collect()
        };
        let mut docs: Vec<Hit> = if len < 2 {
            Vec::new()
        } else {
            // Já vêm do mais recente para o mais velho; a ordenação é estável.
            self.docs
                .iter()
                .enumerate()
                .filter_map(|(i, d)| {
                    let t = tier(&d.key, &q, &words)?;
                    Some(hit(Target::Doc(i), d.name.clone(), t, launches(&format!("doc:{}", d.path)), 0))
                })
                .collect()
        };
        for list in [&mut apps, &mut settings, &mut actions] {
            list.sort_by(|a, b| a.rank().cmp(&b.rank()).then_with(|| a.text.cmp(&b.text)));
        }
        docs.sort_by_key(Hit::rank);

        // Melhor resultado: a conta, se a busca for uma; senão o primeiro mais bem colocado.
        let mut groups = [("Aplicativos", apps), ("Configurações", settings), ("Ações", actions), ("Documentos", docs)];
        if let Some((copy, shown)) = calc::eval(query) {
            self.best = Some(hit(Target::Calc(copy), shown, 0, 0, 0));
            self.best_label = "Calculadora";
        } else if let Some(g) = (0..groups.len())
            .filter(|&g| !groups[g].1.is_empty())
            .min_by_key(|&g| (groups[g].1[0].rank(), g))
        {
            self.best = Some(groups[g].1.remove(0));
            self.best_label = "Melhor resultado";
        }
        for ((_, list), max) in groups.iter_mut().zip([MAX_APPS, MAX_SETTINGS, MAX_ACTIONS, MAX_DOCS]) {
            list.truncate(max);
        }
        groups[3].1.push(hit(Target::Find, format!("{}: {}", tr("Buscar nos arquivos"), query.trim()), 9, 0, 0));
        self.groups = groups.into_iter().filter(|(_, l)| !l.is_empty()).collect();
    }

    fn rows(&self) -> impl Iterator<Item = &Hit> {
        self.best.iter().chain(self.groups.iter().flat_map(|(_, l)| l))
    }

    /// Resto do nome do melhor resultado depois do que foi digitado (autocompletar).
    fn completion(&self) -> String {
        let Some(best) = &self.best else { return String::new() };
        if !matches!(best.target, Target::App(_) | Target::Setting(_) | Target::Action(_) | Target::Doc(_)) {
            return String::new();
        }
        let (name, q) = (&best.text, &self.query);
        // Só se dobrar acentos não mudou o tamanho (o resto é contado em letras).
        let (fname, fq) = (fold(name), fold(q));
        if q.is_empty() || fname.chars().count() != name.chars().count() || !fname.starts_with(&fq) {
            return String::new();
        }
        name.chars().skip(fq.chars().count()).collect()
    }

    fn item(&self, h: &Hit, pos: usize, big: bool) -> Item {
        let (icon, glyph) = match &h.target {
            Target::App(i) => (icon_or_request(&app_source(&self.all[*i]), icon_px(big)), 0xE71D),
            Target::Doc(i) => (icon_or_request(&self.docs[*i].path, icon_px(big)), 0xE8A5),
            Target::Setting(_) => (None, 0xE713),
            Target::Action(i) => (None, ACTIONS[*i].1),
            Target::Calc(_) => (None, 0xE8EF),
            Target::Find => (None, 0xE721),
        };
        // Na linha selecionada, o que o Enter faz (e o Tab, se houver o que completar).
        let hint = if pos != self.sel {
            String::new()
        } else {
            let enter = tr(match h.target {
                Target::Calc(_) => "Enter copia",
                Target::Action(_) => "Enter executa",
                Target::Find => "Enter busca",
                _ => "Enter abre",
            });
            if pos == 0 && self.best.is_some() && !self.completion().is_empty() {
                format!("{}  ·  {enter}", tr("Tab completa"))
            } else {
                enter
            }
        };
        // App selecionado: botão para abrir como administrador (o mesmo do Ctrl+Shift+Enter).
        let button = (pos == self.sel && matches!(h.target, Target::App(_))).then(|| (0xEA18, tr("Admin"), RUN_AS_ADMIN));
        Item { id: ROW + pos as u32, icon, glyph, text: h.text.clone(), hint, button }
    }

    /// Abre a linha `pos` (entre todas). `admin`: app como administrador (verbo `runas`, o
    /// mesmo do menu Iniciar; o Windows pede a confirmação do UAC).
    fn run(&self, pos: usize, admin: bool) {
        let Some(h) = self.rows().nth(pos) else { return };
        let open = |target: &str| unsafe {
            ShellExecuteW(None, w!("open"), &HSTRING::from(target), None, None, SW_SHOWNORMAL);
        };
        let open_app = |target: &str| unsafe {
            let verb = if admin { w!("runas") } else { w!("open") };
            ShellExecuteW(None, verb, &HSTRING::from(target), None, None, SW_SHOWNORMAL);
        };
        let target = h.target.clone();
        let query = self.query.trim().to_string();
        let (all, docs) = (self.all.clone(), self.docs.clone());
        panel::close();
        match target {
            Target::App(i) => {
                count_launch(app_key(&all[i]));
                open_app(&app_source(&all[i]));
            }
            Target::Doc(i) => {
                count_launch(format!("doc:{}", docs[i].path));
                open(&docs[i].path);
            }
            Target::Setting(i) => {
                count_launch(format!("set:{}", SETTINGS[i].2));
                open(SETTINGS[i].2);
            }
            Target::Action(i) => {
                count_launch(format!("act:{}", ACTIONS[i].2));
                power::run(ACTIONS[i].0);
            }
            Target::Calc(text) => copy_text(&text),
            Target::Find => {
                let home = std::env::var("USERPROFILE").unwrap_or_default();
                open(&format!("search-ms:query={}&crumb=location:{}", encode(&query), encode(&home)));
            }
        }
    }
}

/// Codifica para endereço (`%XX` em tudo que não é letra ou número ASCII).
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// Põe `text` na área de transferência.
fn copy_text(text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    unsafe {
        if OpenClipboard(None).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) {
            let p = GlobalLock(mem) as *mut u16;
            if p.is_null() {
                let _ = GlobalFree(Some(mem));
            } else {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
                let _ = GlobalUnlock(mem);
                // Com sucesso, a memória passa a ser do Windows.
                if SetClipboardData(CF_UNICODETEXT, Some(HANDLE(mem.0))).is_err() {
                    let _ = GlobalFree(Some(mem));
                }
            }
        }
        let _ = CloseClipboard();
    }
}

impl PanelContent for Launcher {
    fn width(&self) -> f32 {
        680.0
    }
    fn centered(&self) -> bool {
        true
    }
    fn padded(&self) -> bool {
        false
    }
    fn liquid(&self) -> bool {
        true
    }
    fn notify(&mut self, msg: u32, _wparam: usize) -> bool {
        msg == WM_ICONS
    }
    fn build(&mut self) -> Vec<Widget> {
        let mut out = vec![Widget::Search {
            id: FIELD,
            placeholder: "Apps, configurações, arquivos ou cálculos".into(),
            glyph: 0xE721,
            completion: self.completion(),
        }];
        if self.query.trim().is_empty() {
            let Some((label, apps)) = self.groups.first() else { return out };
            let n = apps.len();
            let end = (self.top + VISIBLE).min(n);
            let items = (self.top..end).map(|pos| self.item(&apps[pos], pos, false)).collect();
            let scroll = (n > VISIBLE).then(|| (self.top as f32 / n as f32, VISIBLE as f32 / n as f32));
            out.push(Widget::Label((*label).into()));
            out.push(Widget::List {
                items,
                rows: VISIBLE.min(n),
                selected: (n > 0).then(|| self.sel - self.top),
                scroll,
                big: false,
            });
            out.push(Widget::Space(10.0));
            return out;
        }
        let sel_in = |start: usize, n: usize| (self.sel >= start && self.sel < start + n).then(|| self.sel - start);
        let mut pos = 0;
        if let Some(best) = &self.best {
            out.push(Widget::Label(self.best_label.into()));
            let items = vec![self.item(best, 0, true)];
            out.push(Widget::List { items, rows: 1, selected: sel_in(0, 1), scroll: None, big: true });
            pos = 1;
        }
        for (label, list) in &self.groups {
            out.push(Widget::Label((*label).into()));
            let items = list.iter().enumerate().map(|(k, h)| self.item(h, pos + k, false)).collect();
            out.push(Widget::List { items, rows: list.len(), selected: sel_in(pos, list.len()), scroll: None, big: false });
            pos += list.len();
        }
        // Folga para a última linha não encostar no canto arredondado.
        out.push(Widget::Space(10.0));
        out
    }
    fn event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Text(FIELD, t) => {
                self.filter(t);
                true
            }
            Event::Submit(FIELD, _) => {
                // Ctrl+Shift+Enter: como administrador, igual ao menu Iniciar do Windows.
                let down = |vk: VIRTUAL_KEY| unsafe { GetKeyState(vk.0 as i32) } < 0;
                self.run(self.sel, down(VK_CONTROL) && down(VK_SHIFT));
                false
            }
            Event::Click(RUN_AS_ADMIN) => {
                self.run(self.sel, true);
                false
            }
            Event::Click(id) if id >= ROW => {
                self.run((id - ROW) as usize, false);
                false
            }
            Event::Key(k) => {
                let n = self.rows().count();
                if n == 0 {
                    return false;
                }
                if k == VK_UP.0 {
                    self.sel = self.sel.saturating_sub(1);
                } else if k == VK_DOWN.0 {
                    self.sel = (self.sel + 1).min(n - 1);
                }
                // Lista de apps sem busca: rola para a seleção ficar à vista.
                if self.sel < self.top {
                    self.top = self.sel;
                } else if self.sel >= self.top + VISIBLE {
                    self.top = self.sel + 1 - VISIBLE;
                }
                true
            }
            _ => false,
        }
    }
}

/// Contas (`2+3*4`, `(1,5+2)^2`) e conversões de unidade (`10 km em mi`, `30 c para f`).
mod calc {
    /// `(texto copiado, texto mostrado)`, ou `None` se a busca não é conta nem conversão.
    pub fn eval(input: &str) -> Option<(String, String)> {
        // Vírgula decimal em português (ou se a busca usou vírgula).
        let pt = !crate::i18n::is_english() && !crate::i18n::is_traditional_chinese();
        let comma = pt || input.contains(',');
        if let Some((v, unit)) = convert(input) {
            let n = format(v, comma, 4);
            return Some((n.clone(), format!("= {n} {unit}")));
        }
        let v = expression(input)?;
        let n = format(v, comma, 8);
        Some((n.clone(), format!("= {n}")))
    }

    fn format(v: f64, comma: bool, decimals: usize) -> String {
        let s = if v.fract() == 0.0 && v.abs() < 1e15 {
            format!("{}", v as i64)
        } else if v.abs() >= 1e15 || v.abs() < 1e-6 {
            format!("{v:e}")
        } else {
            let s = format!("{v:.decimals$}");
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        };
        if comma { s.replace('.', ",") } else { s }
    }

    /// Conta com + - * x / ^ e parênteses; precisa de pelo menos uma operação.
    fn expression(input: &str) -> Option<f64> {
        let chars: Vec<char> = input.chars().filter(|c| !c.is_whitespace()).collect();
        if !chars.iter().any(|c| c.is_ascii_digit()) {
            return None;
        }
        let mut p = Parser { s: &chars, i: 0, ops: 0 };
        let v = p.sum()?;
        (p.i == chars.len() && p.ops > 0 && v.is_finite()).then_some(v)
    }

    struct Parser<'a> {
        s: &'a [char],
        i: usize,
        ops: usize,
    }

    impl Parser<'_> {
        fn peek(&self) -> Option<char> {
            self.s.get(self.i).copied()
        }
        fn sum(&mut self) -> Option<f64> {
            let mut v = self.product()?;
            while let Some(c @ ('+' | '-')) = self.peek() {
                self.i += 1;
                self.ops += 1;
                let r = self.product()?;
                v = if c == '+' { v + r } else { v - r };
            }
            Some(v)
        }
        fn product(&mut self) -> Option<f64> {
            let mut v = self.unary()?;
            while let Some(c @ ('*' | 'x' | 'X' | '×' | '/' | '÷')) = self.peek() {
                self.i += 1;
                self.ops += 1;
                let r = self.unary()?;
                v = if matches!(c, '/' | '÷') { v / r } else { v * r };
            }
            Some(v)
        }
        fn unary(&mut self) -> Option<f64> {
            if self.peek() == Some('-') {
                self.i += 1;
                return Some(-self.unary()?);
            }
            let base = self.atom()?;
            if self.peek() == Some('^') {
                self.i += 1;
                self.ops += 1;
                return Some(base.powf(self.unary()?));
            }
            Some(base)
        }
        fn atom(&mut self) -> Option<f64> {
            if self.peek() == Some('(') {
                self.i += 1;
                let v = self.sum()?;
                if self.peek() != Some(')') {
                    return None;
                }
                self.i += 1;
                return Some(v);
            }
            let start = self.i;
            while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '.' || c == ',') {
                self.i += 1;
            }
            let num: String = self.s[start..self.i].iter().map(|&c| if c == ',' { '.' } else { c }).collect();
            num.parse().ok()
        }
    }

    /// Unidades: (nomes, grandeza, fator para a unidade base). Temperatura (grandeza 3) é à parte.
    const UNITS: [(&[&str], u8, f64); 20] = [
        (&["m", "metro", "metros"], 0, 1.0),
        (&["km", "quilometro", "quilometros"], 0, 1000.0),
        (&["cm", "centimetro", "centimetros"], 0, 0.01),
        (&["mm", "milimetro", "milimetros"], 0, 0.001),
        (&["mi", "milha", "milhas", "mile", "miles"], 0, 1609.344),
        (&["ft", "pe", "pes", "feet", "foot"], 0, 0.3048),
        (&["in", "pol", "polegada", "polegadas", "inch", "inches"], 0, 0.0254),
        (&["yd", "jarda", "jardas"], 0, 0.9144),
        (&["g", "grama", "gramas"], 1, 1.0),
        (&["kg", "quilo", "quilos"], 1, 1000.0),
        (&["lb", "lbs", "libra", "libras"], 1, 453.59237),
        (&["oz", "onca", "oncas"], 1, 28.349523125),
        (&["l", "litro", "litros"], 2, 1.0),
        (&["ml", "mililitro", "mililitros"], 2, 0.001),
        (&["gal", "galao", "galoes"], 2, 3.785411784),
        (&["c", "°c", "celsius"], 3, 0.0),
        (&["f", "°f", "fahrenheit"], 3, 1.0),
        (&["k", "kelvin"], 3, 2.0),
        (&["km/h", "kmh"], 4, 1.0),
        (&["mph"], 4, 1.609344),
    ];

    fn unit(name: &str) -> Option<(u8, f64)> {
        UNITS.iter().find(|(names, ..)| names.contains(&name)).map(|&(_, g, f)| (g, f))
    }

    /// `10 km em mi` → (6.21..., "mi").
    fn convert(input: &str) -> Option<(f64, String)> {
        let text = super::fold(input.trim());
        // Separa número e unidade grudados ("10km").
        let mut spaced = String::new();
        let mut prev_digit = false;
        for c in text.chars() {
            if prev_digit && (c.is_alphabetic() || c == '°') {
                spaced.push(' ');
            }
            prev_digit = c.is_ascii_digit();
            spaced.push(c);
        }
        let t: Vec<&str> = spaced.split_whitespace().collect();
        let (num, from, to) = match t.as_slice() {
            [n, a, b] => (n, a, b),
            [n, a, sep, b] if ["em", "para", "to", "in", "->", "="].contains(sep) => (n, a, b),
            _ => return None,
        };
        let v: f64 = num.replace(',', ".").parse().ok()?;
        let ((ga, fa), (gb, fb)) = (unit(from)?, unit(to)?);
        if ga != gb || from == to {
            return None;
        }
        let r = if ga == 3 {
            let kelvin = match fa as u8 {
                0 => v + 273.15,
                1 => (v - 32.0) * 5.0 / 9.0 + 273.15,
                _ => v,
            };
            match fb as u8 {
                0 => kelvin - 273.15,
                1 => (kelvin - 273.15) * 9.0 / 5.0 + 32.0,
                _ => kelvin,
            }
        } else {
            v * fa / fb
        };
        let shown = match (ga, fb as u8) {
            (3, 0) => "°C".to_string(),
            (3, 1) => "°F".to_string(),
            (3, _) => "K".to_string(),
            _ => to.to_string(),
        };
        Some((r, shown))
    }

    #[cfg(test)]
    mod tests {
        use super::eval;

        /// Resultado com ponto decimal, qualquer que seja o idioma.
        fn shown(q: &str) -> Option<String> {
            eval(q).map(|(_, s)| s.replace(',', "."))
        }

        #[test]
        fn contas() {
            assert_eq!(shown("2+3*4").as_deref(), Some("= 14"));
            assert_eq!(shown("(1,5+2)^2").as_deref(), Some("= 12.25"));
            assert_eq!(shown("10 / 4").as_deref(), Some("= 2.5"));
            assert_eq!(shown("-3x-2").as_deref(), Some("= 6"));
            assert_eq!(shown("2^3^2").as_deref(), Some("= 512"));
            assert_eq!(shown("2024"), None);
            assert_eq!(shown("chrome"), None);
            assert_eq!(shown("2+"), None);
        }

        #[test]
        fn conversoes() {
            assert_eq!(shown("10 km em mi").as_deref(), Some("= 6.2137 mi"));
            assert_eq!(shown("100c para f").as_deref(), Some("= 212 °F"));
            assert_eq!(shown("1 kg lb").as_deref(), Some("= 2.2046 lb"));
            assert_eq!(shown("10 km em kg"), None);
        }
    }
}
