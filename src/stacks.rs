//! Itens fixos da ponta direita do dock, depois de um separador (como no macOS):
//! a pasta Downloads (abre uma pilha com os arquivos mais recentes) e a Lixeira
//! (ícone vazia/cheia, clique abre, botão direito esvazia).

use std::os::windows::fs::MetadataExt;
use std::path::PathBuf;
use std::time::SystemTime;

use tiny_skia::Pixmap;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_UP};
use windows::Win32::UI::Shell::{
    FOLDERID_Downloads, KF_FLAG_DEFAULT, SHEmptyRecycleBinW, SHGetKnownFolderPath, SHQUERYRBINFO,
    SHQueryRecycleBinW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, MF_SEPARATOR, MF_STRING, SW_SHOWNORMAL, SetForegroundWindow,
    TPM_BOTTOMALIGN, TPM_CENTERALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::apps::{App, load_icon};
use crate::panel::{self, Event, PanelContent, Widget};

pub const DOWNLOADS: &str = "::downloads";
pub const TRASH: &str = "::trash";
/// Ícones do tema MacTahoe instalado no PC (preferidos aos do Windows).
const THEME: &str = r"C:\Icones\MacTahoe\sistema";
/// A Lixeira na pasta do shell (ícone de reserva e abrir).
const TRASH_SHELL: &str = "::{645FF040-5081-101B-9F08-00AA002F954E}";

pub struct Stacks {
    /// Downloads e Lixeira, nessa ordem.
    pub items: Vec<App>,
    trash_full: bool,
    icon_size: i32,
}

impl Stacks {
    pub fn new(icon_size: i32) -> Self {
        let item = |source: &str| App {
            source: source.to_string(),
            key: source.to_string(),
            pinned: true,
            icon: None,
            windows: Vec::new(),
        };
        let mut s = Self { items: vec![item(DOWNLOADS), item(TRASH)], trash_full: trash_full(), icon_size };
        s.reload_icons();
        s
    }

    pub fn reload_icons(&mut self) {
        let downloads = downloads_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let trash = if self.trash_full { "user-trash-full" } else { "user-trash" };
        self.items[0].icon = themed("folder-download", &downloads, self.icon_size);
        self.items[1].icon = themed(trash, TRASH_SHELL, self.icon_size);
    }

    /// Lixeira mudou de vazia para cheia (ou o contrário), ou os ícones do tema mudaram.
    /// Retorna true se precisa redesenhar.
    pub fn refresh(&mut self, icons_changed: bool) -> bool {
        let full = trash_full();
        if full == self.trash_full && !icons_changed {
            return false;
        }
        self.trash_full = full;
        self.reload_icons();
        true
    }
}

/// Ícone do tema, se existir; senão o do Windows para `fallback`.
fn themed(name: &str, fallback: &str, size: i32) -> Option<Pixmap> {
    let ico = PathBuf::from(THEME).join(format!("{name}.ico"));
    ico.exists()
        .then(|| load_icon(&ico.to_string_lossy(), size))
        .flatten()
        .or_else(|| load_icon(fallback, size))
}

fn trash_full() -> bool {
    let mut info = SHQUERYRBINFO { cbSize: size_of::<SHQUERYRBINFO>() as u32, ..Default::default() };
    unsafe { SHQueryRecycleBinW(PCWSTR::null(), &mut info) }.is_ok() && info.i64NumItems > 0
}

fn downloads_dir() -> Option<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as _));
        s.map(PathBuf::from)
    }
}

fn open(target: &str) {
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(target), None, None, SW_SHOWNORMAL);
    }
}

fn open_trash() {
    unsafe {
        ShellExecuteW(None, w!("open"), w!("explorer.exe"), w!("shell:RecycleBinFolder"), None, SW_SHOWNORMAL);
    }
}

fn open_downloads() {
    if let Some(dir) = downloads_dir() {
        open(&dir.to_string_lossy());
    }
}

/// Clique no item: a Lixeira abre no Explorador; Downloads abre a pilha acima do ícone.
/// `anchor`: ponto acima do centro do ícone (em coordenadas de tela).
pub fn click(source: &str, anchor: POINT) {
    match source {
        TRASH => open_trash(),
        DOWNLOADS => {
            let item = RECT { left: anchor.x, top: anchor.y, right: anchor.x, bottom: anchor.y };
            panel::toggle("downloads", item, || {
                let scale = unsafe { GetDpiForSystem() } as f32 / 96.0;
                Box::new(Pile { files: recent_files(), top: 0, icon_px: (24.0 * scale).round() as i32, icons: Vec::new() })
            });
        }
        _ => {}
    }
}

const MENU_OPEN: usize = 1;
const MENU_EMPTY: usize = 2;

/// Menu do botão direito do item (Abrir; na Lixeira, também Esvaziar).
pub fn context_menu(hwnd: HWND, source: &str, anchor: POINT) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN, w!("Abrir"));
        if source == TRASH {
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let flags = if trash_full() { MF_STRING } else { MF_STRING | windows::Win32::UI::WindowsAndMessaging::MF_GRAYED };
            let _ = AppendMenuW(menu, flags, MENU_EMPTY, w!("Esvaziar Lixeira"));
        }
        let _ = SetForegroundWindow(hwnd);
        let flags = TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_CENTERALIGN;
        let cmd = TrackPopupMenu(menu, flags, anchor.x, anchor.y, None, hwnd, None);
        let _ = DestroyMenu(menu);
        match (cmd.0 as usize, source) {
            (MENU_OPEN, TRASH) => open_trash(),
            (MENU_OPEN, _) => open_downloads(),
            // Com a confirmação do Windows (sem flags).
            (MENU_EMPTY, _) => {
                let _ = SHEmptyRecycleBinW(Some(hwnd), PCWSTR::null(), 0);
            }
            _ => {}
        }
    }
}

/// Quantos arquivos recentes a pilha guarda e quantas linhas mostra (o resto rola).
const MAX_FILES: usize = 30;
const VISIBLE: usize = 8;
/// Ids: ROW + posição na lista visível; EXPLORER abre a pasta.
const ROW: u32 = 100;
const EXPLORER: u32 = 1;

/// Arquivos e pastas da Downloads, do mais novo para o mais antigo (sem ocultos nem
/// downloads em andamento).
fn recent_files() -> Vec<(PathBuf, String)> {
    let Some(dir) = downloads_dir() else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<(SystemTime, PathBuf, String)> = rd
        .filter_map(|e| {
            let e = e.ok()?;
            let meta = e.metadata().ok()?;
            let name = e.file_name().to_string_lossy().into_owned();
            let lower = name.to_lowercase();
            // 0x2 = oculto, 0x4 = sistema (desktop.ini).
            if meta.file_attributes() & 0x6 != 0 || [".crdownload", ".part", ".tmp"].iter().any(|x| lower.ends_with(x)) {
                return None;
            }
            // Criação = quando chegou na pasta.
            let t = meta.created().or_else(|_| meta.modified()).ok()?;
            Some((t, e.path(), name))
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files.into_iter().take(MAX_FILES).map(|(_, p, n)| (p, n)).collect()
}

/// Pilha da Downloads: lista dos arquivos recentes no painel, acima do ícone.
struct Pile {
    files: Vec<(PathBuf, String)>,
    top: usize,
    icon_px: i32,
    /// Ícones já carregados, por posição em `files`.
    icons: Vec<Option<Option<Pixmap>>>,
}

impl Pile {
    fn icon(&mut self, i: usize) -> Option<Pixmap> {
        self.icons.resize(self.files.len(), None);
        let (path, px) = (&self.files[i].0, self.icon_px);
        self.icons[i].get_or_insert_with(|| load_icon(&path.to_string_lossy(), px)).clone()
    }
}

impl PanelContent for Pile {
    fn centered(&self) -> bool {
        true
    }
    fn above(&self) -> bool {
        true
    }
    fn build(&mut self) -> Vec<Widget> {
        let n = self.files.len();
        let end = (self.top + VISIBLE).min(n);
        let items = (self.top..end)
            .map(|i| (ROW + (i - self.top) as u32, self.icon(i), self.files[i].1.clone()))
            .collect();
        let scroll = (n > VISIBLE).then(|| (self.top as f32 / n as f32, VISIBLE as f32 / n as f32));
        let mut out = vec![Widget::Label("Downloads".into())];
        if n == 0 {
            out.push(Widget::Text("A pasta está vazia.".into()));
        } else {
            out.push(Widget::List { items, rows: VISIBLE.min(n), selected: None, scroll });
        }
        out.push(Widget::Separator);
        out.push(Widget::Row { id: EXPLORER, glyph: Some(0xE838), text: "Abrir no Explorador".into(), sub: None, selected: false });
        out
    }
    fn event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Click(EXPLORER) => {
                panel::close();
                open_downloads();
                false
            }
            Event::Click(id) if id >= ROW => {
                if let Some((path, _)) = self.files.get(self.top + (id - ROW) as usize) {
                    let path = path.to_string_lossy().into_owned();
                    panel::close();
                    open(&path);
                }
                false
            }
            // Roda do mouse e setas rolam a lista.
            Event::Key(k) => {
                let max = self.files.len().saturating_sub(VISIBLE);
                let top = if k == VK_UP.0 {
                    self.top.saturating_sub(1)
                } else if k == VK_DOWN.0 {
                    (self.top + 1).min(max)
                } else {
                    self.top
                };
                std::mem::replace(&mut self.top, top) != top
            }
            _ => false,
        }
    }
}
