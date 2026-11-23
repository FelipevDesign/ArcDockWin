//! Host da área de notificação (bandeja do sistema).
//!
//! Mesma ideia do ManagedShell/Cairo Shell: criamos uma janela nossa da classe "Shell_TrayWnd"
//! e a mantemos acima da do Explorer no z-order. `Shell_NotifyIcon` (shell32) acha a janela com
//! FindWindow("Shell_TrayWnd") e manda WM_COPYDATA para ela, então os ícones chegam aqui.
//! Tudo que recebemos é repassado à janela verdadeira do Explorer: as mensagens de AppBar
//! (o próprio dock é uma AppBar e usa auto-hide), Shell_NotifyIconGetRect e os ícones também,
//! para o Explorer continuar consistente e nada quebrar se o dock sair.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;

use tiny_skia::{IntSize, Pixmap};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GdiFlush, SelectObject,
};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{GUID, w};

pub enum TrayButton {
    Left,
    Right,
    Double,
}

#[derive(Clone)]
pub struct TrayIcon {
    pub icon: Option<Pixmap>,
    pub tip: String,
    /// Identidade estável entre reinícios: `exe#uid` (ex.: `discord.exe#1`).
    pub key: String,
}

/// NOTIFYICONDATAW como o shell32 manda no WM_COPYDATA. Os handles (hWnd, hIcon, hBalloonIcon)
/// vão sempre com 32 bits, mesmo em processos de 64 bits, para o mesmo formato servir a apps
/// WOW64 (handles de USER cabem em 32 bits). Layout conferido no ManagedShell
/// (NativeMethods.Shell32.cs): 956 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
struct NotifyIconData32 {
    cb_size: u32,
    hwnd: u32,
    uid: u32,
    flags: u32,
    callback: u32,
    hicon: u32,
    tip: [u16; 128],
    state: u32,
    state_mask: u32,
    info: [u16; 256],
    version: u32, // união com uTimeout
    info_title: [u16; 64],
    info_flags: u32,
    guid: GUID,
    balloon_icon: u32,
}

/// Cabeçalho que o shell32 põe antes do NOTIFYICONDATA (WM_COPYDATA com dwData == 1).
#[repr(C)]
#[derive(Clone, Copy)]
struct ShellTrayData {
    signature: u32, // sempre 0x34753423
    message: u32,   // NIM_*
    nid: NotifyIconData32,
}

const _: () = assert!(size_of::<NotifyIconData32>() == 956);

/// Shell_NotifyIconGetRect (WM_COPYDATA com dwData == 3). O shell32 manda duas vezes:
/// `message` 1 pede o canto esquerdo/superior e 2 a largura/altura, cada um como MAKELPARAM.
/// Layout do ManagedShell (WINNOTIFYICONIDENTIFIER).
#[repr(C)]
#[derive(Clone, Copy)]
struct IconIdentifier {
    signature: u32,
    message: u32,
    cb_size: u32,
    padding: u32,
    hwnd: u32,
    uid: u32,
    guid: GUID,
}

const TRAY_SIGNATURE: u32 = 0x3475_3423;
const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIM_SETFOCUS: u32 = 3;
const NIM_SETVERSION: u32 = 4;
const NIF_MESSAGE: u32 = 0x01;
const NIF_ICON: u32 = 0x02;
const NIF_TIP: u32 = 0x04;
const NIF_STATE: u32 = 0x08;
const NIF_GUID: u32 = 0x20;
const NIS_HIDDEN: u32 = 0x01;
const NIN_SELECT: u32 = WM_USER;
const NOTIFYICON_VERSION_4: u32 = 4;

const TIMER_TOPMOST: usize = 1;
const TIMER_REBROADCAST: usize = 2;
const TOPMOST_MS: u32 = 200;
/// Segundo pedido de registro dos ícones depois de iniciar (ver `start`).
const REBROADCAST_ON_START_MS: u32 = 3000;
/// A limpeza de ícones órfãos roda a cada N ticks do timer de topo (~5 s).
const PURGE_TICKS: u32 = 25;
/// Tempo máximo esperando o Explorer ao repassar; SMTO_ABORTIFHUNG corta antes se ele travou.
const FORWARD_TIMEOUT_MS: u32 = 1000;

struct Entry {
    hwnd: u32,
    uid: u32,
    guid: Option<GUID>,
    callback: u32,
    version: u32,
    hidden: bool,
    icon: Option<Pixmap>,
    tip: String,
}

struct State {
    hwnd: HWND,
    icon_px: u32,
    on_change: fn(),
    icons: Vec<Entry>,
    ticks: u32,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
    /// Ligado enquanto nós mesmos fazemos o broadcast de TaskbarCreated (a nossa janela recebe
    /// o próprio broadcast de forma síncrona, na mesma thread).
    static BROADCASTING: Cell<bool> = const { Cell::new(false) };
    static EXPLORER: Cell<isize> = const { Cell::new(0) };
}

fn hwnd32(h: u32) -> HWND {
    // Handles de 32 bits são estendidos com sinal ao voltar para 64 bits (regra do WOW64).
    HWND(h as i32 as isize as *mut c_void)
}

/// Janela Shell_TrayWnd do Explorer (não a nossa), com cache.
fn explorer() -> Option<HWND> {
    let cached = HWND(EXPLORER.get() as *mut c_void);
    if !cached.is_invalid() && unsafe { IsWindow(Some(cached)) }.as_bool() {
        return Some(cached);
    }
    let h = crate::taskbar::explorer_tray()?;
    EXPLORER.set(h.0 as isize);
    Some(h)
}

/// Cria a janela da bandeja na thread atual (precisa ter laço de mensagens) e pede aos apps
/// que registrem os ícones de novo.
pub fn start(icon_px: u32, on_change: fn()) -> windows::core::Result<()> {
    if STATE.with_borrow(|s| s.is_some()) {
        return Ok(());
    }
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: w!("Shell_TrayWnd"),
            ..Default::default()
        };
        RegisterClassW(&wc);
        // 0×0 e invisível: só precisa existir e estar no topo do z-order.
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("Shell_TrayWnd"),
            None,
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        TASKBAR_CREATED.set(RegisterWindowMessageW(w!("TaskbarCreated")));
        STATE.set(Some(State { hwnd, icon_px, on_change, icons: Vec::new(), ticks: 0 }));
        make_topmost(hwnd);
        SetTimer(Some(hwnd), TIMER_TOPMOST, TOPMOST_MS, None);
        // Pede de novo daqui a pouco. O aviso é assíncrono e, num reinício rápido do dock, apps
        // lentos para tratar (ex.: Teams e WhatsApp, com WebView2) ainda processam o aviso do
        // dock anterior e acabam registrando o ícone só no Explorer.
        SetTimer(Some(hwnd), TIMER_REBROADCAST, REBROADCAST_ON_START_MS, None);
    }
    broadcast_taskbar_created();
    Ok(())
}

/// Destrói a janela e avisa os apps para voltarem a registrar os ícones no Explorer.
/// Não chama `on_change`.
pub fn stop() {
    let Some(state) = STATE.take() else { return };
    unsafe {
        let _ = KillTimer(Some(state.hwnd), TIMER_TOPMOST);
        let _ = KillTimer(Some(state.hwnd), TIMER_REBROADCAST);
        let _ = DestroyWindow(state.hwnd);
        if let Ok(instance) = GetModuleHandleW(None) {
            let _ = UnregisterClassW(w!("Shell_TrayWnd"), Some(instance.into()));
        }
    }
    broadcast_taskbar_created();
}

/// Ícones visíveis (sem NIS_HIDDEN), em ordem de chegada.
pub fn icons() -> Vec<TrayIcon> {
    STATE.with_borrow(|s| {
        let Some(s) = s else { return Vec::new() };
        s.icons
            .iter()
            .filter(|e| !e.hidden)
            .map(|e| TrayIcon {
                icon: e.icon.clone(),
                tip: e.tip.clone(),
                key: format!("{}#{}", crate::tint::app_key(hwnd32(e.hwnd)).unwrap_or_default(), e.uid),
            })
            .collect()
    })
}

/// Imita o clique da barra do Windows no ícone `index` de `icons()`. `anchor` em coordenadas de
/// tela (os apps com versão 4 usam para posicionar o menu).
pub fn click(index: usize, button: TrayButton, anchor: POINT) {
    let found = STATE.with_borrow(|s| {
        let e = s.as_ref()?.icons.iter().filter(|e| !e.hidden).nth(index)?;
        Some((e.hwnd, e.uid, e.callback, e.version))
    });
    let Some((h, uid, callback, version)) = found else { return };
    if callback == 0 {
        return;
    }
    let hwnd = hwnd32(h);
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        // Sem isso o menu do app não consegue tomar o foco (e não fecha ao clicar fora).
        let _ = AllowSetForegroundWindow(pid);
        // Clique vindo do painel Bandeja: o menu do app abre por cima e o painel fica.
        crate::panel::allow_guest(pid);
    }
    let msgs: &[u32] = match button {
        TrayButton::Left => &[WM_LBUTTONDOWN, WM_LBUTTONUP, NIN_SELECT],
        TrayButton::Right => &[WM_RBUTTONDOWN, WM_RBUTTONUP, WM_CONTEXTMENU],
        TrayButton::Double => &[WM_LBUTTONDBLCLK],
    };
    for &m in msgs {
        let (wp, lp) = if version >= NOTIFYICON_VERSION_4 {
            // wParam = MAKEWPARAM(x, y); lParam = MAKELPARAM(msg, uID)
            let wp = (anchor.x as u16 as usize) | ((anchor.y as u16 as usize) << 16);
            let lp = (m as u16 as u32 | ((uid as u16 as u32) << 16)) as i32 as isize;
            (wp, lp)
        } else {
            // Versão antiga só conhece as mensagens de mouse.
            if m == NIN_SELECT || m == WM_CONTEXTMENU {
                continue;
            }
            (uid as usize, m as isize)
        };
        unsafe {
            let _ = PostMessageW(Some(hwnd), callback, WPARAM(wp), LPARAM(lp));
        }
    }
}

fn broadcast_taskbar_created() {
    BROADCASTING.set(true);
    unsafe {
        let _ = SendNotifyMessageW(HWND_BROADCAST, TASKBAR_CREATED.get(), WPARAM(0), LPARAM(0));
    }
    BROADCASTING.set(false);
}

fn make_topmost(hwnd: HWND) {
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

/// Dá à nossa Shell_TrayWnd (invisível) o retângulo da barra de cima. Apps como o Logitech
/// G HUB abrem o menu da bandeja junto do retângulo dela; com 0×0 em (0,0) o menu ia para o
/// canto da tela. Só mexe quando a barra mudou.
fn match_topbar(hwnd: HWND) {
    let Some(want) = crate::topbar::main_rect() else { return };
    unsafe {
        let mut now = windows::Win32::Foundation::RECT::default();
        if GetWindowRect(hwnd, &mut now).is_ok() && now == want {
            return;
        }
        let _ = SetWindowPos(
            hwnd,
            None,
            want.left,
            want.top,
            want.right - want.left,
            want.bottom - want.top,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// Repassa a mensagem ao Shell_TrayWnd do Explorer e devolve o resultado dele.
/// SendMessageTimeout em vez de SendMessage para um Explorer travado não travar o dock;
/// sem SMTO_BLOCK, para continuarmos atendendo mensagens enviadas a nós enquanto esperamos
/// (o Explorer manda notificações de AppBar ao dock durante ABM_*).
fn forward(msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    let target = explorer()?;
    let mut result = 0usize;
    let ok = unsafe {
        SendMessageTimeoutW(
            target,
            msg,
            wp,
            lp,
            SMTO_NORMAL | SMTO_ABORTIFHUNG,
            FORWARD_TIMEOUT_MS,
            Some(&mut result),
        )
    };
    (ok.0 != 0).then_some(LRESULT(result as isize))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COPYDATA => {
            let cds = unsafe { &*(lp.0 as *const COPYDATASTRUCT) };
            // Onde está o ícone: quem sabe é a nossa barra. O Explorer responderia com a barra
            // dele, escondida, e o menu do app abriria longe do ícone.
            if cds.dwData == 3
                && let Some(r) = unsafe { icon_rect(cds) }
            {
                return LRESULT(r);
            }
            // Trata o nosso caso antes, sem segurar empréstimo do estado durante o repasse
            // (o SendMessageTimeout pode reentrar neste wndproc).
            let own = if cds.dwData == 1 { unsafe { handle_tray(cds) } } else { None };
            let fwd = forward(msg, wp, lp);
            if let Some((ok, changed)) = own {
                if changed {
                    if let Some(f) = STATE.with_borrow(|s| s.as_ref().map(|s| s.on_change)) {
                        f();
                    }
                }
                return LRESULT(ok as isize);
            }
            return fwd.unwrap_or(LRESULT(0));
        }
        WM_TIMER => {
            match wp.0 {
                TIMER_TOPMOST => on_timer(hwnd),
                TIMER_REBROADCAST => unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER_REBROADCAST);
                    make_topmost(hwnd);
                    broadcast_taskbar_created();
                },
                _ => {}
            }
            return LRESULT(0);
        }
        m if m != 0 && m == TASKBAR_CREATED.get() => {
            // TaskbarCreated que não é nosso: o Explorer reiniciou e os apps estão registrando
            // os ícones agora, parte deles na janela nova do Explorer. Voltamos ao topo e
            // pedimos de novo daqui a pouco, quando a do Explorer já existe.
            if !BROADCASTING.get() {
                EXPLORER.set(0);
                unsafe { SetTimer(Some(hwnd), TIMER_REBROADCAST, 1500, None) };
            }
            return LRESULT(0);
        }
        // O que o ManagedShell repassa: WM_ACTIVATEAPP, WM_COMMAND (ex.: "minimizar tudo") e
        // mensagens privadas (>= WM_USER). As registradas (>= 0xC000) ficam de fora porque em
        // geral chegam por broadcast, que o Explorer já recebe direto.
        WM_ACTIVATEAPP | WM_COMMAND => {
            if let Some(r) = forward(msg, wp, lp) {
                return r;
            }
        }
        m if (WM_USER..0xC000).contains(&m) => {
            // WM_USER + 372 o ManagedShell posta em vez de enviar (o Explorer pode demorar).
            if m == WM_USER + 372 {
                if let Some(target) = explorer() {
                    unsafe {
                        let _ = PostMessageW(Some(target), msg, wp, lp);
                    }
                }
            } else if let Some(r) = forward(msg, wp, lp) {
                return r;
            }
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn on_timer(hwnd: HWND) {
    // Se outra Shell_TrayWnd (a do Explorer, ao reiniciar) passou à frente, voltamos ao topo.
    if unsafe { FindWindowW(w!("Shell_TrayWnd"), None) }.ok() != Some(hwnd) {
        make_topmost(hwnd);
    }
    match_topbar(hwnd);
    // Remove ícones de janelas que morreram sem NIM_DELETE (app fechado à força).
    let changed = STATE.with_borrow_mut(|s| {
        let Some(s) = s else { return None };
        s.ticks += 1;
        if s.ticks < PURGE_TICKS {
            return None;
        }
        s.ticks = 0;
        let mut changed = false;
        s.icons.retain(|e| {
            let alive = unsafe { IsWindow(Some(hwnd32(e.hwnd))) }.as_bool();
            changed |= !alive && !e.hidden;
            alive
        });
        changed.then_some(s.on_change)
    });
    if let Some(f) = changed {
        f();
    }
}

/// Resposta de Shell_NotifyIconGetRect para um ícone nosso: MAKELPARAM do canto ou do tamanho.
/// None se não for nosso (ou a barra não souber), e aí a pergunta vai ao Explorer.
unsafe fn icon_rect(cds: &COPYDATASTRUCT) -> Option<isize> {
    if cds.lpData.is_null() || (cds.cbData as usize) < size_of::<IconIdentifier>() {
        return None;
    }
    let id: IconIdentifier = unsafe { std::ptr::read_unaligned(cds.lpData as *const IconIdentifier) };
    if id.signature != TRAY_SIGNATURE {
        return None;
    }
    let by_guid = id.guid != GUID::zeroed();
    // Mesmo índice de `icons()` (só os sem NIS_HIDDEN), que é o da barra.
    let index = STATE.with_borrow(|s| {
        s.as_ref()?.icons.iter().filter(|e| !e.hidden).position(|e| {
            if by_guid { e.guid == Some(id.guid) } else { e.hwnd == id.hwnd && e.uid == id.uid }
        })
    })?;
    let r = crate::topbar::tray_icon_rect(index)?;
    let (x, y) = match id.message {
        1 => (r.left, r.top),
        // O shell32 soma ao canto: aqui vão largura e altura (testado; não o canto oposto).
        2 => (r.right - r.left, r.bottom - r.top),
        _ => return None,
    };
    Some(((x as u16 as u32) | ((y as u16 as u32) << 16)) as i32 as isize)
}

/// Aplica um Shell_NotifyIcon à nossa lista. Devolve (resultado para o app, mudou algo visível)
/// ou None se a mensagem não tem o formato esperado.
unsafe fn handle_tray(cds: &COPYDATASTRUCT) -> Option<(bool, bool)> {
    if cds.lpData.is_null() || (cds.cbData as usize) < 8 + 6 * 4 {
        return None;
    }
    // Apps antigos mandam NOTIFYICONDATA menor; copiamos o que veio num struct zerado.
    let mut data: ShellTrayData = unsafe { std::mem::zeroed() };
    let n = (cds.cbData as usize).min(size_of::<ShellTrayData>());
    unsafe { std::ptr::copy_nonoverlapping(cds.lpData as *const u8, &mut data as *mut _ as *mut u8, n) };
    if data.signature != TRAY_SIGNATURE {
        return None;
    }
    let nid = &data.nid;
    STATE.with_borrow_mut(|s| {
        let s = s.as_mut()?;
        let pos = if nid.flags & NIF_GUID != 0 {
            s.icons.iter().position(|e| e.guid == Some(nid.guid))
        } else {
            s.icons.iter().position(|e| e.hwnd == nid.hwnd && e.uid == nid.uid)
        };
        Some(match data.message {
            NIM_ADD | NIM_MODIFY => {
                let i = match pos {
                    Some(i) => i,
                    // MODIFY de ícone desconhecido falha, como no Explorer.
                    None if data.message == NIM_MODIFY => return Some((false, false)),
                    None => {
                        s.icons.push(Entry {
                            hwnd: nid.hwnd,
                            uid: nid.uid,
                            guid: None,
                            callback: 0,
                            version: 0,
                            hidden: false,
                            icon: None,
                            tip: String::new(),
                        });
                        s.icons.len() - 1
                    }
                };
                let px = s.icon_px;
                let e = &mut s.icons[i];
                let was_hidden = pos.is_some() && e.hidden;
                apply(e, nid, px);
                (true, !(was_hidden && e.hidden))
            }
            NIM_DELETE => match pos {
                Some(i) => {
                    let e = s.icons.remove(i);
                    (true, !e.hidden)
                }
                None => (false, false),
            },
            NIM_SETVERSION => match pos {
                Some(i) => {
                    s.icons[i].version = nid.version;
                    (true, false)
                }
                None => (false, false),
            },
            NIM_SETFOCUS => (true, false),
            _ => (false, false),
        })
    })
}

fn apply(e: &mut Entry, nid: &NotifyIconData32, px: u32) {
    if nid.flags & NIF_MESSAGE != 0 {
        e.callback = nid.callback;
    }
    if nid.flags & NIF_ICON != 0 {
        // Convertido já aqui, enquanto o app está parado no SendMessage e o HICON dele é
        // garantidamente válido; por isso não guardamos cópia (CopyIcon) do handle.
        e.icon = if nid.hicon == 0 {
            None
        } else {
            render_icon(HICON(nid.hicon as i32 as isize as *mut c_void), px)
        };
    }
    if nid.flags & NIF_TIP != 0 {
        let len = nid.tip.iter().position(|&c| c == 0).unwrap_or(nid.tip.len());
        e.tip = String::from_utf16_lossy(&nid.tip[..len]);
    }
    if nid.flags & NIF_STATE != 0 && nid.state_mask & NIS_HIDDEN != 0 {
        e.hidden = nid.state & NIS_HIDDEN != 0;
    }
    if nid.flags & NIF_GUID != 0 {
        e.guid = Some(nid.guid);
    }
}

/// Desenha o ícone numa DIB 32 bpp de px×px e devolve um Pixmap pré-multiplicado.
fn render_icon(hicon: HICON, px: u32) -> Option<Pixmap> {
    let size = px as i32;
    let n = (px * px) as usize;
    unsafe {
        let hdc = CreateCompatibleDC(None);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size, // de cima para baixo
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let Ok(dib) = CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(hdc);
            return None;
        };
        let old = SelectObject(hdc, dib.into());
        let buf = std::slice::from_raw_parts_mut(bits as *mut u8, n * 4);

        // Sobre fundo transparente (zero), o DrawIconEx de um ícone com alfa já sai pré-multiplicado.
        buf.fill(0);
        let _ = DrawIconEx(hdc, 0, 0, hicon, size, size, 0, None, DI_NORMAL);
        let _ = GdiFlush();
        let mut out = buf.to_vec();
        if out.chunks_exact(4).all(|p| p[3] == 0) {
            // Ícone sem canal alfa: a transparência vem da máscara AND. Desenhada (SRCAND) sobre
            // branco, fica branca onde é transparente e preta onde é opaco.
            buf.fill(0xFF);
            let _ = DrawIconEx(hdc, 0, 0, hicon, size, size, 0, None, DI_MASK);
            let _ = GdiFlush();
            for (p, m) in out.chunks_exact_mut(4).zip(buf.chunks_exact(4)) {
                if m[0] != 0 {
                    p.copy_from_slice(&[0, 0, 0, 0]);
                } else {
                    p[3] = 0xFF;
                }
            }
        }
        SelectObject(hdc, old);
        let _ = DeleteObject(dib.into());
        let _ = DeleteDC(hdc);

        // BGRA -> RGBA, garantindo cor <= alfa (exigência do formato pré-multiplicado).
        for p in out.chunks_exact_mut(4) {
            let a = p[3];
            let (b, g, r) = (p[0].min(a), p[1].min(a), p[2].min(a));
            p[0] = r;
            p[1] = g;
            p[2] = b;
        }
        Pixmap::from_vec(out, IntSize::from_wh(px, px)?)
    }
}
