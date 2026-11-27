//! Pré-visualização rápida (como o Peek do PowerToys / Quick Look do macOS): com um arquivo
//! selecionado no Explorador (ou na Área de Trabalho), Espaço abre a janela de pré-visualização.
//!
//! O dock só mantém um gancho de teclado leve (numa thread própria) com a opção ligada. A
//! janela em si é outro processo (`arcdockwin.exe --peek`, `peek.rs`), que só existe enquanto
//! está aberta: ao fechar, toda a memória dela volta para o sistema.
//!
//! O Espaço só é usado com o foco na lista de arquivos (renomear, barra de endereço e busca
//! continuam normais) e nunca enquanto o Peek do PowerToys estiver rodando.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IServiceProvider};
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_SPACE};
use windows::Win32::UI::Shell::{
    IFolderView2, IShellBrowser, IShellItemArray, IShellWindows, IWebBrowserApp, SID_STopLevelBrowser, SIGDN_FILESYSPATH,
    SVGIO_ALLVIEW, SVGIO_FLAG_VIEWORDER, SVGIO_SELECTION, ShellWindows,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, Interface};

static THREAD: AtomicU32 = AtomicU32::new(0);
/// Peek do PowerToys rodando (checado de tempos em tempos, não a cada tecla).
static PEEK_RUNNING: AtomicBool = AtomicBool::new(false);
/// O Espaço apertado foi nosso: o soltar também não passa.
static SWALLOWED: AtomicBool = AtomicBool::new(false);

pub fn sync() {
    let want = crate::extras::on("ql_on");
    if !want {
        let tid = THREAD.swap(0, Ordering::AcqRel);
        if tid != 0 && tid != u32::MAX {
            unsafe {
                let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
    } else if THREAD.compare_exchange(0, u32::MAX, Ordering::AcqRel, Ordering::Acquire).is_ok() {
        std::thread::spawn(hook_thread);
    }
}

fn hook_thread() {
    unsafe {
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        if THREAD.compare_exchange(u32::MAX, GetCurrentThreadId(), Ordering::AcqRel, Ordering::Acquire).is_err() {
            return;
        }
        check_peek();
        let timer = SetTimer(None, 0, 5000, Some(peek_timer));
        let module = GetModuleHandleW(None).ok().map(Into::into);
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_proc), module, 0);
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }
        let _ = KillTimer(None, timer);
        if let Ok(h) = hook {
            let _ = UnhookWindowsHookEx(h);
        }
    }
}

unsafe extern "system" fn peek_timer(_: HWND, _: u32, _: usize, _: u32) {
    check_peek();
}

fn check_peek() {
    let mut found = false;
    unsafe {
        if let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            let mut ok = Process32FirstW(snap, &mut e).is_ok();
            while ok {
                let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                let name = String::from_utf16_lossy(&e.szExeFile[..n]).to_lowercase();
                if name == "powertoys.peek.ui.exe" {
                    found = true;
                    break;
                }
                ok = Process32NextW(snap, &mut e).is_ok();
            }
            let _ = windows::Win32::Foundation::CloseHandle(snap);
        }
    }
    PEEK_RUNNING.store(found, Ordering::Relaxed);
}

fn down(vk: u16) -> bool {
    unsafe { GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0 }
}

fn class_of(h: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(h, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n])
}

/// Explorador (ou Área de Trabalho) em foco, com o foco do teclado na lista de arquivos.
fn explorer_list() -> Option<HWND> {
    unsafe {
        let fg = GetForegroundWindow();
        let cls = class_of(fg);
        if !matches!(cls.as_str(), "CabinetWClass" | "ExploreWClass" | "Progman" | "WorkerW") {
            return None;
        }
        let tid = GetWindowThreadProcessId(fg, None);
        let mut gi = GUITHREADINFO { cbSize: size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        GetGUIThreadInfo(tid, &mut gi).ok()?;
        // Renomeando um arquivo há um caret/campo de edição: deixa o Espaço passar.
        if !gi.hwndCaret.is_invalid() || class_of(gi.hwndFocus) != "DirectUIHWND" {
            return None;
        }
        Some(fg)
    }
}

unsafe extern "system" fn key_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32 {
            let k = &*(lp.0 as *const KBDLLHOOKSTRUCT);
            let injected = k.flags.0 & 0x10 != 0;
            if k.vkCode as u16 == VK_SPACE.0 && !injected {
                match wp.0 as u32 {
                    WM_KEYDOWN | WM_SYSKEYDOWN => {
                        let ctrl = down(VK_CONTROL.0);
                        let others = down(VK_MENU.0) || down(VK_SHIFT.0) || down(VK_LWIN.0) || down(VK_RWIN.0);
                        let want_ctrl = crate::extras::on("ql_ctrl");
                        if ctrl == want_ctrl && !others && !PEEK_RUNNING.load(Ordering::Relaxed) {
                            // Repetição da tecla segurada: continua engolindo, sem abrir de novo.
                            if SWALLOWED.load(Ordering::Relaxed) {
                                return LRESULT(1);
                            }
                            if let Some(fg) = explorer_list() {
                                SWALLOWED.store(true, Ordering::Relaxed);
                                let fg = fg.0 as isize;
                                std::thread::spawn(move || open_for(HWND(fg as _)));
                                return LRESULT(1);
                            }
                        }
                    }
                    WM_KEYUP | WM_SYSKEYUP if SWALLOWED.swap(false, Ordering::Relaxed) => return LRESULT(1),
                    _ => {}
                }
            }
        }
        CallNextHookEx(None, code, wp, lp)
    }
}

/// Lê a seleção do Explorador `fg` e abre a janela de pré-visualização.
fn open_for(fg: HWND) {
    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        if let Some((list, index)) = selection(fg) {
            launch(&list, index);
        }
        if init {
            CoUninitialize();
        }
    }
}

/// Caminhos para navegar e o índice do selecionado. Vários selecionados: navega entre eles;
/// um só: entre todos os itens da pasta, na ordem da tela.
fn selection(fg: HWND) -> Option<(Vec<String>, usize)> {
    unsafe {
        let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL).ok()?;
        let desktop = matches!(class_of(fg).as_str(), "Progman" | "WorkerW");
        let mut view: Option<IFolderView2> = None;
        if desktop {
            let mut hwnd = 0i32;
            let loc = VARIANT::from(0x0000i32); // CSIDL_DESKTOP
            let disp = windows
                .FindWindowSW(&loc, &VARIANT::default(), windows::Win32::UI::Shell::SWC_DESKTOP, &mut hwnd, windows::Win32::UI::Shell::SWFO_NEEDDISPATCH)
                .ok()?;
            view = folder_view(&disp.cast().ok()?);
        } else {
            let n = windows.Count().ok()?;
            for i in 0..n {
                let Ok(disp) = windows.Item(&VARIANT::from(i)) else { continue };
                let Ok(app) = disp.cast::<IWebBrowserApp>() else { continue };
                if app.HWND().map(|h| h.0 as isize) == Ok(fg.0 as isize) {
                    view = folder_view(&disp.cast().ok()?);
                    break;
                }
            }
        }
        let view = view?;
        let selected: IShellItemArray = view.GetSelection(false).ok()?;
        let paths = |arr: &IShellItemArray, max: u32| -> Vec<String> {
            let n = arr.GetCount().unwrap_or(0).min(max);
            (0..n)
                .filter_map(|i| {
                    let it = arr.GetItemAt(i).ok()?;
                    let p = it.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
                    let s = p.to_string().ok();
                    windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as _));
                    s
                })
                .collect()
        };
        let sel = paths(&selected, 500);
        if sel.is_empty() {
            return None;
        }
        if sel.len() > 1 {
            return Some((sel, 0));
        }
        let all: Option<IShellItemArray> = view.Items(windows::Win32::UI::Shell::_SVGIO(SVGIO_ALLVIEW.0 | SVGIO_FLAG_VIEWORDER.0)).ok();
        let _ = SVGIO_SELECTION;
        let all = all.map(|a| paths(&a, 3000)).unwrap_or_default();
        match all.iter().position(|p| *p == sel[0]) {
            Some(i) => Some((all, i)),
            None => Some((sel, 0)),
        }
    }
}

fn folder_view(sp: &IServiceProvider) -> Option<IFolderView2> {
    unsafe {
        let browser: IShellBrowser = sp.QueryService(&SID_STopLevelBrowser).ok()?;
        let view = browser.QueryActiveShellView().ok()?;
        view.cast().ok()
    }
}

/// Grava a lista num arquivo temporário e abre o processo da janela.
fn launch(list: &[String], index: usize) {
    let file = std::env::temp_dir().join("arcdock-peek.txt");
    let mut text = format!("{index}\n");
    for p in list {
        text.push_str(p);
        text.push('\n');
    }
    if std::fs::write(&file, text).is_err() {
        return;
    }
    // Já aberta: troca o conteúdo em vez de abrir outra.
    unsafe {
        if let Ok(h) = FindWindowW(windows::core::w!("ArcDockPeek"), None) {
            let _ = PostMessageW(Some(h), WM_APP + 1, WPARAM(0), LPARAM(0));
            let _ = SetForegroundWindow(h);
            return;
        }
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let _ = std::process::Command::new(exe).arg("--peek").arg(&file).spawn();
    let _ = HSTRING::new();
}
