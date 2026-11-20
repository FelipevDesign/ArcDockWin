//! Cores dos apps guardadas em SQLite (`%APPDATA%\ArcDockWin\colors.db`), usando o SQLite
//! que vem com o Windows (winsqlite3.dll): sem crate nem binário extra.
//!
//! Tabela `colors(app TEXT PRIMARY KEY, color TEXT)`: `app` é o nome do exe em minúsculas
//! (ex.: `brave.exe`) e `color` é `#RRGGBB`. Pode ser editada à mão; vale na próxima leitura.
//!
//! Tabela `tray(icon TEXT PRIMARY KEY, shown INTEGER)`: ícones da bandeja que ficam na barra
//! (`shown` = 1) ou dentro da bandeja (0). `icon` é `exe#uid` (ex.: `discord.exe#1`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};

#[allow(non_camel_case_types)]
type sqlite3 = c_void;
#[allow(non_camel_case_types)]
type sqlite3_stmt = c_void;

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
/// Faz o SQLite copiar o texto passado em `bind_text`.
const SQLITE_TRANSIENT: isize = -1;

#[link(name = "winsqlite3")]
unsafe extern "system" {
    fn sqlite3_open(filename: *const c_char, db: *mut *mut sqlite3) -> c_int;
    fn sqlite3_close(db: *mut sqlite3) -> c_int;
    fn sqlite3_exec(
        db: *mut sqlite3,
        sql: *const c_char,
        cb: *const c_void,
        arg: *mut c_void,
        err: *mut *mut c_char,
    ) -> c_int;
    fn sqlite3_prepare_v2(
        db: *mut sqlite3,
        sql: *const c_char,
        n: c_int,
        stmt: *mut *mut sqlite3_stmt,
        tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_bind_text(stmt: *mut sqlite3_stmt, i: c_int, text: *const c_char, n: c_int, free: isize) -> c_int;
    fn sqlite3_step(stmt: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_column_text(stmt: *mut sqlite3_stmt, col: c_int) -> *const u8;
    fn sqlite3_finalize(stmt: *mut sqlite3_stmt) -> c_int;
}

struct Db(*mut sqlite3);

impl Drop for Db {
    fn drop(&mut self) {
        unsafe { sqlite3_close(self.0) };
    }
}

thread_local! {
    static DB: RefCell<Option<Db>> = const { RefCell::new(None) };
}

fn open() -> Option<Db> {
    let dir = std::path::PathBuf::from(std::env::var_os("APPDATA")?).join("ArcDockWin");
    let _ = std::fs::create_dir_all(&dir);
    let path = CString::new(dir.join("colors.db").to_str()?).ok()?;
    let mut db = std::ptr::null_mut();
    if unsafe { sqlite3_open(path.as_ptr(), &mut db) } != SQLITE_OK {
        unsafe { sqlite3_close(db) };
        return None;
    }
    let db = Db(db);
    let sql = c"CREATE TABLE IF NOT EXISTS colors (app TEXT PRIMARY KEY, color TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS tray (icon TEXT PRIMARY KEY, shown INTEGER NOT NULL);";
    let ok = unsafe { sqlite3_exec(db.0, sql.as_ptr(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok == SQLITE_OK).then_some(db)
}

fn with_db<R>(f: impl FnOnce(*mut sqlite3) -> Option<R>) -> Option<R> {
    DB.with(|d| {
        let mut d = d.borrow_mut();
        if d.is_none() {
            *d = open();
        }
        f(d.as_ref()?.0)
    })
}

/// Roda `sql` com `args` ligados a ?1, ?2â€¦; devolve a 1ª coluna da 1ª linha, se houver.
fn run(db: *mut sqlite3, sql: &CStr, args: &[&str]) -> Option<Option<String>> {
    unsafe {
        let mut stmt = std::ptr::null_mut();
        if sqlite3_prepare_v2(db, sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut()) != SQLITE_OK {
            return None;
        }
        for (i, a) in args.iter().enumerate() {
            sqlite3_bind_text(stmt, i as c_int + 1, a.as_ptr() as *const c_char, a.len() as c_int, SQLITE_TRANSIENT);
        }
        let out = if sqlite3_step(stmt) == SQLITE_ROW {
            let p = sqlite3_column_text(stmt, 0);
            (!p.is_null()).then(|| CStr::from_ptr(p as *const c_char).to_string_lossy().into_owned())
        } else {
            None
        };
        sqlite3_finalize(stmt);
        Some(out)
    }
}

/// Cor guardada para o app, se houver.
pub fn get(app: &str) -> Option<[u8; 3]> {
    // Cache em memória: chamado a cada troca de app.
    if let Some(c) = CACHE.with_borrow(|m| m.get(app).copied()) {
        return c;
    }
    let c = load(app);
    CACHE.with_borrow_mut(|m| m.insert(app.to_owned(), c));
    c
}

thread_local! {
    static CACHE: RefCell<HashMap<String, Option<[u8; 3]>>> = RefCell::new(HashMap::new());
}

fn load(app: &str) -> Option<[u8; 3]> {
    let hex = with_db(|db| run(db, c"SELECT color FROM colors WHERE app = ?1", &[app]).flatten())?;
    let hex = hex.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// Guarda (ou troca) a cor do app.
pub fn set(app: &str, [r, g, b]: [u8; 3]) {
    CACHE.with_borrow_mut(|m| m.insert(app.to_owned(), Some([r, g, b])));
    let hex = format!("#{r:02X}{g:02X}{b:02X}");
    with_db(|db| run(db, c"INSERT OR REPLACE INTO colors (app, color) VALUES (?1, ?2)", &[app, &hex]));
}

/// Se o ícone da bandeja fica na barra; None se o usuário nunca escolheu.
pub fn tray_shown(icon: &str) -> Option<bool> {
    let v = with_db(|db| run(db, c"SELECT shown FROM tray WHERE icon = ?1", &[icon]).flatten())?;
    Some(v.trim() != "0")
}

/// Guarda se o ícone da bandeja fica na barra.
pub fn set_tray_shown(icon: &str, shown: bool) {
    with_db(|db| run(db, c"INSERT OR REPLACE INTO tray (icon, shown) VALUES (?1, ?2)", &[icon, if shown { "1" } else { "0" }]));
}
