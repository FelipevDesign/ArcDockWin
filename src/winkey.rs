//! Tecla Windows sozinha abre o lançador em vez do menu Iniciar. Win com outra tecla (Win+E,
//! Win+D...) segue normal, e o Ctrl+Esc continua abrindo o Iniciar do Windows.
//!
//! Limite do Windows: com uma janela de administrador em foco o gancho não recebe as teclas,
//! e aí o Iniciar abre como sempre.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_LWIN,
    VK_RWIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, FindWindowW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG, PM_NOREMOVE, PeekMessageW,
    PostMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP,
    WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
};
use windows::core::w;

/// Tecla sem uso: apertada junto com a Win, o Windows não abre o Iniciar ao soltar.
const MASK: VIRTUAL_KEY = VIRTUAL_KEY(0xE8);

/// Thread do gancho (0 = desligado; u32::MAX = subindo).
static THREAD: AtomicU32 = AtomicU32::new(0);
static WIN_DOWN: AtomicBool = AtomicBool::new(false);
/// Outra tecla foi apertada com a Win segurada.
static COMBO: AtomicBool = AtomicBool::new(false);

/// Liga ou desliga o gancho conforme a opção (no início e quando ela muda).
pub fn config_changed() {
    if !crate::config::win_key_launcher() {
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

/// Gancho numa thread só dele: responde na hora mesmo com o dock ocupado desenhando.
fn hook_thread() {
    unsafe {
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE); // cria a fila antes de publicar o id
        if THREAD.compare_exchange(u32::MAX, GetCurrentThreadId(), Ordering::AcqRel, Ordering::Acquire).is_err() {
            return; // desligado enquanto subia
        }
        let module = GetModuleHandleW(None).ok().map(Into::into);
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_proc), module, 0);
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
        if let Ok(h) = hook {
            let _ = UnhookWindowsHookEx(h);
        }
    }
}

unsafe extern "system" fn key_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32 {
            let vk = (*(lp.0 as *const KBDLLHOOKSTRUCT)).vkCode as u16;
            let win = vk == VK_LWIN.0 || vk == VK_RWIN.0;
            match wp.0 as u32 {
                WM_KEYDOWN | WM_SYSKEYDOWN if win => {
                    // A Win repete enquanto segurada: só o primeiro aperto conta.
                    if !WIN_DOWN.swap(true, Ordering::Relaxed) {
                        COMBO.store(false, Ordering::Relaxed);
                    }
                }
                WM_KEYUP | WM_SYSKEYUP if win => {
                    if WIN_DOWN.swap(false, Ordering::Relaxed) && !COMBO.load(Ordering::Relaxed) {
                        // Segura o soltar e reenvia depois da máscara: injetada aqui a máscara
                        // chegaria depois do soltar, e o Iniciar já teria aberto.
                        release_masked(VIRTUAL_KEY(vk));
                        open_launcher();
                        return LRESULT(1);
                    }
                }
                WM_KEYDOWN | WM_SYSKEYDOWN if vk != MASK.0 && WIN_DOWN.load(Ordering::Relaxed) => {
                    COMBO.store(true, Ordering::Relaxed);
                }
                _ => {}
            }
        }
        CallNextHookEx(None, code, wp, lp)
    }
}

/// Aperta e solta a tecla sem uso e só então solta a Win: para o Windows ela não estava "sozinha".
fn release_masked(win: VIRTUAL_KEY) {
    let key = |vk, flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, dwFlags: flags, ..Default::default() } },
    };
    let seq = [key(MASK, KEYBD_EVENT_FLAGS(0)), key(MASK, KEYEVENTF_KEYUP), key(win, KEYEVENTF_KEYUP | KEYEVENTF_EXTENDEDKEY)];
    unsafe {
        SendInput(&seq, size_of::<INPUT>() as i32);
    }
}

fn open_launcher() {
    unsafe {
        // Os docks são recriados (monitores, configurações): procura a janela na hora.
        if let Ok(dock) = FindWindowW(w!("ArcDockWin"), None) {
            let _ = PostMessageW(Some(dock), crate::WM_LAUNCHER, WPARAM(0), LPARAM(0));
        }
    }
}
