//! Animação de minimizar janelas até o ícone do dock (efeitos Gênio e Escala).
//!
//! Quando o evento de minimizar chega a janela já sumiu (a captura sai preta), então a foto é
//! tirada antes, sem injetar DLL: no clique do botão "_" (gancho de mouse de baixo nível), no
//! Win+↓ (gancho de teclado) e no clique do dock que minimiza o app em foco. Só anima se o
//! minimizar daquela janela vier em até 2 s. Nesse intervalo a animação nativa fica desligada
//! (sem gravar no perfil); o valor original fica em disco para o vigia restaurar se o dock morrer.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::time::Instant;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CAPTION_BUTTON_BOUNDS, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC,
    CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP, HDC, HGDIOBJ, SelectObject,
};
use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_DOWN, VK_LWIN, VK_RWIN};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

const WM_CAPTURE: u32 = WM_APP + 1;
/// Botão solto em cima do "_": mostra já a cópia ao vivo por cima (o minimizar vem em seguida).
const WM_ARM: u32 = WM_APP + 2;
const TIMER_PENDING: usize = 1;
/// Prazo para o minimizar acontecer depois da foto (ms).
const PENDING_MS: u32 = 2000;
/// Duração da animação (segundos).
const DURATION: f32 = 0.5;
/// Momento do primeiro quadro da animação (um quadro de 60 Hz, em segundos).
const FIRST: f32 = 0.0167;

/// Janela da animação (HWND como número).
static OVERLAY: AtomicIsize = AtomicIsize::new(0);
/// Thread dos ganchos de baixo nível: 0 = desligados, u32::MAX = subindo.
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);

/// Foto da janela (BGRA em u32, alfa pré-multiplicado) no retângulo visível dela.
struct Shot {
    hwnd: HWND,
    w: usize,
    h: usize,
    px: Vec<u32>,
    rect: RECT,
}

/// Esperando o minimizar: a foto, ou já a animação pronta (quadro 0 enviado, janela escondida).
enum Pending {
    Shot(Shot),
    Ready(Anim),
}

impl Pending {
    fn hwnd(&self) -> HWND {
        match self {
            Pending::Shot(s) => s.hwnd,
            Pending::Ready(a) => a.shot.hwnd,
        }
    }
}

struct Anim {
    shot: Shot,
    /// Quadrado do ícone no dock, em coordenadas da tela.
    target: RECT,
    genie: bool,
    start: Instant,
    /// Já desenhou o primeiro quadro (o relógio começa nele).
    running: bool,
    /// Canto da janela da animação e o quadro (DIB) do tamanho dela.
    origin: POINT,
    w: usize,
    h: usize,
    dc: HDC,
    dib: HBITMAP,
    bits: *mut u32,
    /// Parte do quadro com desenho (x0, y0, x1, y1): só ela é desenhada e mandada à tela.
    /// A janela inteira de tela cheia, a cada quadro, era o que travava a animação.
    area: std::cell::Cell<(usize, usize, usize, usize)>,
}

impl Drop for Anim {
    fn drop(&mut self) {
        unsafe { free_dib(self.dc, self.dib) };
    }
}

thread_local! {
    static PENDING: RefCell<Option<Pending>> = const { RefCell::new(None) };
    static ANIM: RefCell<Option<Anim>> = const { RefCell::new(None) };
    static STARTING: Cell<bool> = const { Cell::new(false) };
}

/// Janela da animação (a barra do topo ignora os eventos dela).
pub fn overlay() -> HWND {
    HWND(OVERLAY.load(Ordering::Relaxed) as *mut c_void)
}

/// Cria a janela da animação e liga os ganchos (chamado uma vez, na thread do dock).
pub fn init() {
    restore_native();
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return };
        let class = w!("ArcDockWinMinimize");
        let wc = WNDCLASSW { lpfnWndProc: Some(overlay_proc), hInstance: module.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE;
        let Ok(h) = CreateWindowExW(ex, class, class, WS_POPUP, 0, 0, 0, 0, None, None, Some(module.into()), None) else {
            return;
        };
        OVERLAY.store(h.0 as isize, Ordering::Relaxed);
        let _ = SetWinEventHook(
            EVENT_SYSTEM_MINIMIZESTART,
            EVENT_SYSTEM_MINIMIZESTART,
            None,
            Some(on_minimize),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        );
    }
    config_changed();
}

/// Saída do dock: desliga os ganchos e devolve a animação nativa.
pub fn shutdown() {
    stop_hooks();
    restore_native();
}

/// Chamado pela tela de Configurações quando `animcfg` muda (liga/desliga ganchos etc.).
pub fn config_changed() {
    if !crate::animcfg::enabled() {
        stop_hooks();
    } else if HOOK_THREAD.compare_exchange(0, u32::MAX, Ordering::AcqRel, Ordering::Acquire).is_ok() {
        std::thread::spawn(hook_thread);
    }
}

fn stop_hooks() {
    let tid = HOOK_THREAD.swap(0, Ordering::AcqRel);
    if tid != 0 && tid != u32::MAX {
        unsafe {
            let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
}

/// Animação em andamento: o dock fica visível e o laço principal desenha um quadro por vez.
pub fn busy() -> bool {
    STARTING.get() || ANIM.with(|a| a.borrow().is_some()) || thumb::running()
}

/// Ganchos numa thread só deles: respondem na hora mesmo com o dock ocupado desenhando.
fn hook_thread() {
    unsafe {
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE); // cria a fila antes de publicar o id
        if HOOK_THREAD.compare_exchange(u32::MAX, GetCurrentThreadId(), Ordering::AcqRel, Ordering::Acquire).is_err() {
            return; // desligado enquanto subia
        }
        let module = GetModuleHandleW(None).ok().map(Into::into);
        let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module, 0);
        let keys = SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_proc), module, 0);
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
        for h in [mouse, keys].into_iter().flatten() {
            let _ = UnhookWindowsHookEx(h);
        }
    }
}

/// Botão esquerdo apertado sobre o "_" de uma janela: pede a foto (o minimizar só vem ao soltar).
unsafe extern "system" fn mouse_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32 && wp.0 as u32 == WM_LBUTTONUP {
            let _ = PostMessageW(Some(overlay()), WM_ARM, WPARAM(0), LPARAM(0));
        }
        if code == HC_ACTION as i32 && wp.0 as u32 == WM_LBUTTONDOWN {
            let pt = (*(lp.0 as *const MSLLHOOKSTRUCT)).pt;
            let root = GetAncestor(WindowFromPoint(pt), GA_ROOT);
            let lparam = LPARAM((pt.x as u16 as isize) | ((pt.y as u16 as isize) << 16));
            let mut hit = 0usize;
            if !root.is_invalid()
                && SendMessageTimeoutW(root, WM_NCHITTEST, WPARAM(0), lparam, SMTO_ABORTIFHUNG, 30, Some(&mut hit)).0 != 0
                && (hit == HTMINBUTTON as usize
                    // Explorador do Windows 11 (barra com abas numa janela-filha) responde
                    // HTCLIENT em cima do "_": a posição dos botões do DWM decide.
                    || ((hit == HTCAPTION as usize || hit == HTCLIENT as usize) && on_dwm_min_button(root, pt)))
            {
                let _ = PostMessageW(Some(overlay()), WM_CAPTURE, WPARAM(root.0 as usize), LPARAM(0));
            }
        }
        CallNextHookEx(None, code, wp, lp)
    }
}

/// Moldura padrão desenhada pelo DWM: o WM_NCHITTEST mandado de fora às vezes responde
/// HTCAPTION (ou HTCLIENT, no Explorador) em cima do "_". Confere pelo primeiro terço da área
/// dos botões (_ □ X). Um falso positivo só tira uma foto à toa: sem minimizar em 2 s, nada anima.
unsafe fn on_dwm_min_button(hwnd: HWND, pt: POINT) -> bool {
    unsafe {
        if GetWindowLongW(hwnd, GWL_STYLE) as u32 & WS_MINIMIZEBOX.0 == 0 {
            return false;
        }
        let (mut wr, mut b) = (RECT::default(), RECT::default());
        let size = size_of::<RECT>() as u32;
        if GetWindowRect(hwnd, &mut wr).is_err()
            || DwmGetWindowAttribute(hwnd, DWMWA_CAPTION_BUTTON_BOUNDS, &mut b as *mut _ as *mut c_void, size).is_err()
        {
            return false;
        }
        let (x, y) = (pt.x - wr.left, pt.y - wr.top);
        b.right > b.left && x >= b.left && x < b.left + (b.right - b.left) / 3 && y >= b.top && y < b.bottom
    }
}

/// Win+↓ na janela em foco (maximizada, o Win+↓ só restaura: nada a fazer).
unsafe extern "system" fn key_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let down = matches!(wp.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        if code == HC_ACTION as i32 && down && (*(lp.0 as *const KBDLLHOOKSTRUCT)).vkCode == VK_DOWN.0 as u32 {
            let win = |k: u16| GetAsyncKeyState(k as i32) as u16 & 0x8000 != 0;
            let fg = GetForegroundWindow();
            if (win(VK_LWIN.0) || win(VK_RWIN.0)) && !fg.is_invalid() && !IsZoomed(fg).as_bool() {
                let _ = PostMessageW(Some(overlay()), WM_CAPTURE, WPARAM(fg.0 as usize), LPARAM(1));
            }
        }
        CallNextHookEx(None, code, wp, lp)
    }
}

unsafe extern "system" fn overlay_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CAPTURE => capture_with(HWND(wp.0 as *mut c_void), lp.0 == 1),
            WM_ARM => {
                if thumb::busy() && !thumb::running() {
                    thumb::arm();
                    // Soltou: o minimizar vem logo; se não vier, a cópia não fica por cima.
                    SetTimer(Some(hwnd), TIMER_PENDING, 700, None);
                }
            }
            WM_TIMER if wp.0 == TIMER_PENDING => {
                // O minimizar não veio (soltou fora do botão, Win+↓ só restaurou...).
                let _ = KillTimer(Some(hwnd), TIMER_PENDING);
                PENDING.with(|p| p.borrow_mut().take());
                thumb::cancel();
                restore_if_idle();
            }
            _ => return DefWindowProcW(hwnd, msg, wp, lp),
        }
        LRESULT(0)
    }
}

/// Prepara a animação de `hwnd` (o dock vai minimizá-la agora).
pub fn capture(hwnd: HWND) {
    capture_with(hwnd, true);
}

/// Prepara a animação e desliga a nativa até o minimizar vir. Primeiro tenta a cópia ao vivo
/// do Windows (DWM: a placa de vídeo desenha, sem foto e sem piscar); sem ela, a foto antiga.
/// `arm` = mostra já a cópia por cima (sem esperar o botão do mouse soltar).
fn capture_with(hwnd: HWND, arm: bool) {
    if !crate::animcfg::enabled() || overlay().is_invalid() {
        return;
    }
    unsafe {
        if IsIconic(hwnd).as_bool() || !IsWindowVisible(hwnd).as_bool() {
            return;
        }
        if thumb::prepare(hwnd) {
            PENDING.with(|p| p.borrow_mut().take());
            disable_native();
            if arm {
                thumb::arm();
            }
            SetTimer(Some(overlay()), TIMER_PENDING, PENDING_MS, None);
            return;
        }
        let Some(shot) = snapshot(hwnd) else { return };
        disable_native();
        // Pré-aquece já no aperto: quadro e janela da animação prontos, o primeiro quadro sai na hora.
        // (Sem forçar o dock a aparecer: isso só quando o minimizar vier de fato.)
        // Com outra animação na tela a janela está ocupada: aí monta só no início.
        let target = if busy() { None } else { crate::minimize_target(hwnd) };
        let pending = match target {
            Some(target) => build(shot, target).map(Pending::Ready),
            None => Some(Pending::Shot(shot)),
        };
        PENDING.with(|p| *p.borrow_mut() = pending);
        SetTimer(Some(overlay()), TIMER_PENDING, PENDING_MS, None);
    }
}

/// PrintWindow com PW_RENDERFULLCONTENT (funciona com Chrome/GPU), recortado sem a borda invisível.
unsafe fn snapshot(hwnd: HWND) -> Option<Shot> {
    unsafe {
        let mut wr = RECT::default();
        GetWindowRect(hwnd, &mut wr).ok()?;
        let mut fr = wr;
        let _ = DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut fr as *mut _ as *mut c_void, size_of::<RECT>() as u32);
        // Visível = interseção dos dois (o retângulo do DWM nunca passa do da janela, mas por via das dúvidas).
        let r = RECT { left: fr.left.max(wr.left), top: fr.top.max(wr.top), right: fr.right.min(wr.right), bottom: fr.bottom.min(wr.bottom) };
        let (ww, wh) = ((wr.right - wr.left) as usize, (wr.bottom - wr.top) as usize);
        let (w, h) = ((r.right - r.left).max(0) as usize, (r.bottom - r.top).max(0) as usize);
        if w == 0 || h == 0 {
            return None;
        }
        let (dc, dib, bits) = new_dib(ww, wh)?;
        let ok = PrintWindow(hwnd, dc, PRINT_WINDOW_FLAGS(2)).as_bool();
        let src = std::slice::from_raw_parts(bits, ww * wh);
        let (ox, oy) = ((r.left - wr.left) as usize, (r.top - wr.top) as usize);
        let mut px = Vec::with_capacity(w * h);
        for y in oy..oy + h {
            // O PrintWindow não preenche o alfa: opaco (os cantos arredondados vêm depois).
            px.extend(src[y * ww + ox..][..w].iter().map(|&c| c | 0xFF00_0000));
        }
        free_dib(dc, dib);
        if !ok {
            return None;
        }
        if !IsZoomed(hwnd).as_bool() {
            round_corners(&mut px, w, h, 8.0 * GetDpiForWindow(hwnd) as f32 / 96.0);
        }
        Some(Shot { hwnd, w, h, px, rect: r })
    }
}

fn round_corners(px: &mut [u32], w: usize, h: usize, r: f32) {
    let n = (r.ceil() as usize).min(w / 2).min(h / 2);
    for y in 0..n {
        for x in 0..n {
            let (dx, dy) = (r - x as f32 - 0.5, r - y as f32 - 0.5);
            let cov = (r - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
            if cov >= 1.0 {
                continue;
            }
            for (cx, cy) in [(x, y), (w - 1 - x, y), (x, h - 1 - y), (w - 1 - x, h - 1 - y)] {
                let c = &mut px[cy * w + cx];
                *c = mul(*c, (cov * 256.0) as u32);
            }
        }
    }
}

unsafe fn new_dib(w: usize, h: usize) -> Option<(HDC, HBITMAP, *mut u32)> {
    unsafe {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        let dc = CreateCompatibleDC(None);
        SelectObject(dc, HGDIOBJ(dib.0));
        Some((dc, dib, bits as *mut u32))
    }
}

unsafe fn free_dib(dc: HDC, dib: HBITMAP) {
    unsafe {
        let _ = DeleteDC(dc);
        let _ = DeleteObject(HGDIOBJ(dib.0));
    }
}

/// O minimizar começou: se for a janela fotografada, anima.
unsafe extern "system" fn on_minimize(_: HWINEVENTHOOK, _: u32, hwnd: HWND, _: i32, _: i32, _: u32, _: u32) {
    if thumb::start(hwnd) {
        unsafe {
            let _ = KillTimer(Some(overlay()), TIMER_PENDING);
        }
        return;
    }
    let pending = PENDING.with(|p| {
        let mut p = p.borrow_mut();
        if p.as_ref().is_some_and(|s| s.hwnd() == hwnd) { p.take() } else { None }
    });
    let Some(pending) = pending else { return };
    unsafe {
        let _ = KillTimer(Some(overlay()), TIMER_PENDING);
    }
    start(pending);
}

fn start(pending: Pending) {
    ANIM.with(|a| a.borrow_mut().take());
    // Com `busy()` já verdadeiro o dock aparece (se estiver escondido) ao calcular o destino.
    STARTING.set(true);
    let target = crate::minimize_target(pending.hwnd());
    STARTING.set(false);
    let anim = match pending {
        Pending::Ready(a) => Some(a),
        Pending::Shot(s) => build(s, target.unwrap_or_else(fallback_target)),
    };
    let Some(anim) = anim else {
        restore_if_idle();
        return;
    };
    // Acima do dock: por baixo, o vidro desfocaria a janela (o vidro inteiro fica da cor dela).
    unsafe {
        let _ = SetWindowPos(overlay(), Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
    }
    ANIM.with(|a| *a.borrow_mut() = Some(anim));
}

/// Sem dock acessível: um ícone de 48 px no meio, embaixo.
fn fallback_target() -> RECT {
    let (sw, sh) = crate::taskbar::screen_size();
    RECT { left: sw / 2 - 24, top: sh - 60, right: sw / 2 + 24, bottom: sh - 12 }
}

/// Monta a animação e já manda o quadro 0 para a janela (ainda escondida).
fn build(shot: Shot, target: RECT) -> Option<Anim> {
    let r = shot.rect;
    let origin = POINT { x: r.left.min(target.left), y: r.top.min(target.top) };
    let w = (r.right.max(target.right) - origin.x) as usize;
    let h = (r.bottom.max(target.bottom) - origin.y) as usize;
    let (dc, dib, bits) = unsafe { new_dib(w, h) }?;
    let genie = crate::animcfg::effect() == crate::animcfg::Effect::Genie;
    let anim = Anim {
        shot,
        target,
        genie,
        start: Instant::now(),
        running: false,
        origin,
        w,
        h,
        dc,
        dib,
        bits,
        area: std::cell::Cell::new((0, 0, w, h)),
    };
    anim.render(0.0);
    anim.present();
    // O primeiro quadro em movimento já fica pronto no buffer: com a janela grande ele é o
    // mais caro e atrasaria o segundo. O ULW acima já copiou o quadro 0.
    anim.render(FIRST / DURATION);
    Some(anim)
}

/// Um quadro (chamado pelo laço principal enquanto `busy()`).
pub fn frame() {
    if thumb::running() {
        if thumb::frame() {
            restore_if_idle();
        }
        return;
    }
    let done = ANIM.with(|a| {
        let mut a = a.borrow_mut();
        let Some(a) = a.as_mut() else { return false };
        // O quadro 0 já está na tela desde o início e o primeiro quadro em movimento já está
        // pronto no buffer; o relógio conta a partir dele, então um atraso da thread do dock
        // não vira salto na animação.
        if !a.running {
            a.running = true;
            a.start = Instant::now() - std::time::Duration::from_secs_f32(FIRST);
            a.present();
            return false;
        }
        let t = a.start.elapsed().as_secs_f32() / DURATION;
        if t < 1.0 {
            a.render(t);
            a.present();
        }
        t >= 1.0
    });
    if done {
        // Esconde e libera a foto e o quadro (RAM importa).
        ANIM.with(|a| a.borrow_mut().take());
        unsafe {
            let _ = ShowWindow(overlay(), SW_HIDE);
        }
        restore_if_idle();
    }
}

/// Ease-in-out quíntica (smootherstep): começa e termina sem tranco.
fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// Pixel pré-multiplicado vezes `f` (0..=256), dois canais por vez.
fn mul(c: u32, f: u32) -> u32 {
    (((c & 0x00FF_00FF) * f >> 8) & 0x00FF_00FF) | (((c >> 8) & 0x00FF_00FF) * f & 0xFF00_FF00)
}

/// Mistura de dois pixels, `f` (0..=256) de `b`.
fn mix(a: u32, b: u32, f: u32) -> u32 {
    mul(a, 256 - f).wrapping_add(mul(b, f))
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

impl Anim {
    /// Retângulos da janela e do ícone no quadro, para o desenho (que roda em várias threads).
    fn geo(&self) -> Geo {
        let f = |v: i32, o: i32| (v - o) as f32;
        let (r, g, o) = (&self.shot.rect, &self.target, self.origin);
        Geo {
            win: [f(r.left, o.x), f(r.top, o.y), f(r.right, o.x), f(r.bottom, o.y)],
            icon: [f(g.left, o.x), f(g.top, o.y), f(g.right, o.x), f(g.bottom, o.y)],
            genie: self.genie,
        }
    }

    /// Desenha o quadro `t` (0..1) no buffer. Janela grande: as linhas se dividem entre threads
    /// (intercaladas, porque as de baixo dão mais trabalho no começo do gênio).
    fn render(&self, t: f32) {
        let (w, h) = (self.w, self.h);
        let dst = unsafe { std::slice::from_raw_parts_mut(self.bits, w * h) };
        let (geo, px, sw, sh) = (self.geo(), &self.shot.px[..], self.shot.w, self.shot.h);
        // Só as linhas com algo neste quadro (a forma encolhe rápido): menos contas e, no
        // `present`, bem menos pixels mandados para a tela.
        let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0usize, 0usize);
        for y in 0..h {
            if let Some((_, l, r, a)) = geo.row(t, y as f32 + 0.5)
                && a > 0.002
                && r - l >= 0.01
            {
                y0 = y0.min(y);
                y1 = y + 1;
                x0 = x0.min(l.floor().max(0.0) as usize);
                x1 = x1.max((r.ceil() as usize + 1).min(w));
            }
        }
        if y1 <= y0 || x1 <= x0 {
            (x0, y0, x1, y1) = (0, 0, 1, 1);
        }
        self.area.set((x0, y0, x1, y1));
        let draw = |rows: Vec<(usize, &mut [u32])>| {
            for (y, out) in rows {
                geo.draw_row(t, y, out, px, sw, sh);
            }
        };
        let n = if w * h < 300_000 { 1 } else { std::thread::available_parallelism().map_or(1, |n| n.get().min(4)) };
        let mut parts: Vec<Vec<(usize, &mut [u32])>> = (0..n).map(|_| Vec::with_capacity(h / n + 1)).collect();
        for (y, out) in dst.chunks_exact_mut(w).enumerate().skip(y0).take(y1 - y0) {
            parts[y % n].push((y, out));
        }
        if n == 1 {
            parts.into_iter().for_each(draw);
        } else {
            let draw = &draw;
            std::thread::scope(|sc| {
                for part in parts {
                    sc.spawn(move || draw(part));
                }
            });
        }
    }
}

#[derive(Clone, Copy)]
struct Geo {
    win: [f32; 4],
    icon: [f32; 4],
    genie: bool,
}

impl Geo {
    /// Linha `y` do quadro (centro do pixel): (linha da foto 0..1, borda esquerda, borda direita,
    /// opacidade da linha 0..1, já com a cobertura vertical nas pontas).
    fn row(&self, t: f32, y: f32) -> Option<(f32, f32, f32, f32)> {
        let [wl, wt, wr, wb] = self.win;
        let [il, it, ir, ib] = self.icon;
        // Cobertura da linha entre `top` e `bot` (sem degrau no topo e na base).
        let cover = |top: f32, bot: f32| ((y + 0.5).min(bot) - (y - 0.5).max(top)).clamp(0.0, 1.0);
        if !self.genie {
            // Escala: encolhe (ease-out cúbica) até um pouco menor que o ícone, como se entrasse
            // nele, e some na segunda metade.
            let s = 0.7 * ((ir - il) / (wr - wl)).min((ib - it) / (wb - wt));
            let (cx, cy) = ((il + ir) / 2.0, (it + ib) / 2.0);
            let (hw, hh) = ((wr - wl) * s / 2.0, (wb - wt) * s / 2.0);
            let e = 1.0 - (1.0 - t).powi(3);
            let (top, bot) = (lerp(wt, cy - hh, e), lerp(wb, cy + hh, e));
            let c = cover(top, bot);
            if c <= 0.0 {
                return None;
            }
            let alpha = 1.0 - smooth((t - 0.4) / 0.6);
            return Some(((y - top) / (bot - top), lerp(wl, cx - hw, e), lerp(wr, cx + hw, e), alpha * c));
        }
        // Gênio: a imagem desce por um funil fixo na tela (curva suave da janela até o ícone)
        // e se espreme dentro do ícone.
        let b = smooth((t - 0.2) / 0.8);
        let (top, bot) = (lerp(wt, it, b), lerp(wb, ib, b));
        let c = cover(top, bot);
        if c <= 0.0 {
            return None;
        }
        let u = if it - wt > 1.0 { ((y - wt) / (it - wt)).clamp(0.0, 1.0) } else { 1.0 };
        let s = smooth(u);
        let (fl, fr) = (lerp(wl, il, s), lerp(wr, ir, s));
        // As bordas entortam até o funil, as linhas de baixo primeiro.
        let a = smooth((t - 0.3 * (1.0 - u)) / 0.4);
        // Some ao chegar: pela metade final, e as linhas mais perto do ícone antes.
        let q = if ib - wt > 1.0 { (y - wt) / (ib - wt) } else { 1.0 };
        let alpha = 1.0 - smooth((t - 0.5) / 0.42 + 0.5 * (q - 1.0));
        Some(((y - top) / (bot - top), lerp(wl, fl, a), lerp(wr, fr, a), alpha * c))
    }

    /// Desenha a linha `y` do quadro em `out` a partir da foto `px` (sw x sh).
    fn draw_row(&self, t: f32, y: usize, out: &mut [u32], px: &[u32], sw: usize, sh: usize) {
        out.fill(0);
        let w = out.len();
        {
            let Some((v, l, r, alpha)) = self.row(t, y as f32 + 0.5) else { return };
            if r - l < 0.01 || alpha <= 0.002 {
                return;
            }
            // Amostragem bilinear (pelo centro dos pixels: no quadro 0 sai idêntica à foto),
            // em ponto fixo 16.16 para caber no quadro.
            let sy = (v * sh as f32 - 0.5).clamp(0.0, (sh - 1) as f32);
            let y0 = sy as usize;
            let fy = ((sy - y0 as f32) * 256.0) as u32;
            let (r0, r1) = (&px[y0 * sw..][..sw], &px[(y0 + 1).min(sh - 1) * sw..][..sw]);
            let k = sw as f32 / (r - l);
            let (x0, x1) = (l.floor().max(0.0) as usize, (r.ceil() as usize).min(w));
            let step = (k * 65536.0) as i64;
            let sx = (((x0 as f32 + 0.5 - l) * k - 0.5) * 65536.0) as i64;
            let max = ((sw - 1) as i64) << 16;
            let a = (alpha * 256.0) as u32;
            let sample = |sx: i64| {
                let s = sx.clamp(0, max);
                let (i, fx) = ((s >> 16) as usize, ((s >> 8) & 0xFF) as u32);
                let j = (i + 1).min(sw - 1);
                let c = if fx == 0 { r0[i] } else { mix(r0[i], r0[j], fx) };
                if fy == 0 { c } else { mix(c, if fx == 0 { r1[i] } else { mix(r1[i], r1[j], fx) }, fy) }
            };
            // Miolo (pixels inteiros dentro da linha): só inteiros.
            let (ia, ib) = ((l.ceil().max(0.0) as usize).min(x1), (r.floor().max(0.0) as usize).clamp(x0, x1));
            for (n, o) in out[ia.min(ib)..ib].iter_mut().enumerate() {
                let c = sample(sx + step * (ia - x0 + n) as i64);
                *o = if a < 256 { mul(c, a) } else { c };
            }
            // Pontas com cobertura subpixel (sem serrilhado nem tremor).
            for x in (x0..ia.min(x1)).chain(ib.max(ia)..x1) {
                let xc = x as f32 + 0.5;
                let cov = ((xc + 0.5).min(r) - (xc - 0.5).max(l)).clamp(0.0, 1.0);
                out[x] = mul(sample(sx + step * (x - x0) as i64), (cov * a as f32) as u32);
            }
        }
    }
}

impl Anim {
    /// Manda o buffer para a janela da animação.
    fn present(&self) {
        let (x0, y0, x1, y1) = self.area.get();
        let size = SIZE { cx: (x1 - x0) as i32, cy: (y1 - y0) as i32 };
        let dst = POINT { x: self.origin.x + x0 as i32, y: self.origin.y + y0 as i32 };
        let src = POINT { x: x0 as i32, y: y0 as i32 };
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        // Posição e tamanho em todo quadro: só com o conteúdo o DWM não mostra a atualização.
        unsafe {
            let _ = UpdateLayeredWindow(
                overlay(),
                None,
                Some(&dst),
                Some(&size),
                Some(self.dc),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }
}

// --- Animação nativa de minimizar -------------------------------------------------------

fn state_file() -> std::path::PathBuf {
    crate::apps::config_path().with_file_name("minanimate_state")
}

fn set_min_animate(on: i32) {
    let mut ai = ANIMATIONINFO { cbSize: size_of::<ANIMATIONINFO>() as u32, iMinAnimate: on };
    unsafe {
        // Sem SPIF_UPDATEINIFILE: não persiste no perfil.
        let _ = SystemParametersInfoW(SPI_SETANIMATION, ai.cbSize, Some(&mut ai as *mut _ as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
}

/// Desliga a animação nativa e guarda o valor original em disco (se já não estiver guardado).
fn disable_native() {
    let file = state_file();
    if file.exists() {
        return;
    }
    let mut ai = ANIMATIONINFO { cbSize: size_of::<ANIMATIONINFO>() as u32, iMinAnimate: 0 };
    let got = unsafe { SystemParametersInfoW(SPI_GETANIMATION, ai.cbSize, Some(&mut ai as *mut _ as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) };
    if got.is_err() || ai.iMinAnimate == 0 {
        return;
    }
    if std::fs::write(&file, ai.iMinAnimate.to_string()).is_ok() {
        set_min_animate(0);
    }
}

/// Devolve a animação nativa ao valor guardado e apaga o arquivo. Usado também pelo vigia.
pub fn restore_native() {
    let file = state_file();
    if let Some(v) = std::fs::read_to_string(&file).ok().and_then(|s| s.trim().parse::<i32>().ok()) {
        set_min_animate(v);
    }
    let _ = std::fs::remove_file(file);
}

fn restore_if_idle() {
    if PENDING.with(|p| p.borrow().is_none()) && ANIM.with(|a| a.borrow().is_none()) && !thumb::busy() {
        restore_native();
    }
}

/// Animação pela cópia ao vivo do Windows (miniaturas do DWM). O compositor desenha a janela
/// em pedaços (faixas horizontais no gênio, um só na escala) e o dock só move os retângulos a
/// cada quadro: nada de copiar pixels, então fica leve e acompanha a taxa da tela.
mod thumb {
    use std::cell::RefCell;
    use std::time::Instant;

    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::Graphics::Dwm::*;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::w;

    use super::{DURATION, Geo, lerp, smooth};

    /// Faixas do gênio (mais faixas = curva mais lisa).
    const STRIPS: usize = 40;

    struct Thumb {
        src: HWND,
        /// Retângulo visível da janela (tela) e onde ele começa dentro do retângulo da janela.
        frame: RECT,
        inset: (i32, i32),
        thumbs: Vec<isize>,
        armed: bool,
        start: Option<Instant>,
        geo: Option<Geo>,
        /// Canto da camada (tela virtual).
        origin: (i32, i32),
    }

    thread_local! {
        static T: RefCell<Option<Thumb>> = const { RefCell::new(None) };
        static LAYER: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    }

    unsafe extern "system" fn layer_proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        unsafe {
            if m == WM_NCHITTEST {
                return LRESULT(-1); // HTTRANSPARENT
            }
            DefWindowProcW(h, m, wp, lp)
        }
    }

    /// Camada transparente do tamanho da tela virtual onde as faixas aparecem.
    fn layer() -> Option<HWND> {
        let h = LAYER.get();
        if h != 0 {
            return Some(HWND(h as _));
        }
        unsafe {
            let inst = GetModuleHandleW(None).ok()?;
            let wc = WNDCLASSW { lpfnWndProc: Some(layer_proc), hInstance: inst.into(), lpszClassName: w!("ArcDockWinMinThumb"), ..Default::default() };
            RegisterClassW(&wc);
            let ex = WS_EX_NOREDIRECTIONBITMAP | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE;
            let h = CreateWindowExW(ex, w!("ArcDockWinMinThumb"), w!(""), WS_POPUP, 0, 0, 1, 1, None, None, Some(inst.into()), None).ok()?;
            LAYER.set(h.0 as isize);
            Some(h)
        }
    }

    pub fn busy() -> bool {
        T.with_borrow(|t| t.is_some())
    }

    pub fn running() -> bool {
        T.with_borrow(|t| t.as_ref().is_some_and(|t| t.start.is_some()))
    }

    /// Liga as miniaturas (ainda invisíveis). false = sem DWM: usa a foto.
    pub fn prepare(src: HWND) -> bool {
        cancel();
        let Some(layer) = layer() else { return false };
        unsafe {
            let mut wr = RECT::default();
            let mut fr = RECT::default();
            if GetWindowRect(src, &mut wr).is_err() {
                return false;
            }
            if DwmGetWindowAttribute(src, DWMWA_EXTENDED_FRAME_BOUNDS, &mut fr as *mut _ as *mut _, size_of::<RECT>() as u32).is_err() {
                fr = wr;
            }
            let n = if crate::animcfg::effect() == crate::animcfg::Effect::Genie { STRIPS } else { 1 };
            let mut thumbs = Vec::with_capacity(n);
            for _ in 0..n {
                match DwmRegisterThumbnail(layer, src) {
                    Ok(t) => thumbs.push(t),
                    Err(_) => {
                        for t in thumbs {
                            let _ = DwmUnregisterThumbnail(t);
                        }
                        return false;
                    }
                }
            }
            let (vx, vy) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
            let (vw, vh) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
            let _ = SetWindowPos(layer, Some(HWND_TOPMOST), vx, vy, vw, vh, SWP_NOACTIVATE);
            T.with_borrow_mut(|t| {
                *t = Some(Thumb {
                    src,
                    frame: fr,
                    inset: (fr.left - wr.left, fr.top - wr.top),
                    thumbs,
                    armed: false,
                    start: None,
                    geo: None,
                    origin: (vx, vy),
                })
            });
            true
        }
    }

    /// Mostra a cópia exatamente por cima da janela (fica idêntica): quando ela sumir, a
    /// cópia já está lá e não pisca.
    pub fn arm() {
        let armed = T.with_borrow_mut(|t| {
            let Some(t) = t.as_mut() else { return false };
            if t.armed {
                return false;
            }
            t.armed = true;
            let f = t.frame;
            let (ox, oy) = t.origin;
            let n = t.thumbs.len();
            let h = (f.bottom - f.top) as f32;
            for (i, &th) in t.thumbs.iter().enumerate() {
                let (y0, y1) = ((h * i as f32 / n as f32).round() as i32, (h * (i + 1) as f32 / n as f32).round() as i32);
                set(th, t.inset, (f.right - f.left, y0, y1), RECT { left: f.left - ox, top: f.top - oy + y0, right: f.right - ox, bottom: f.top - oy + y1 }, 255);
            }
            true
        });
        if armed && let Some(l) = layer() {
            unsafe {
                let _ = SetWindowPos(l, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
            }
        }
    }

    /// Faixa `th`: pedaço da janela (largura, linhas y0..y1 do retângulo visível) em `dest`.
    fn set(th: isize, inset: (i32, i32), src: (i32, i32, i32), dest: RECT, opacity: u8) {
        let p = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_RECTSOURCE | DWM_TNP_OPACITY | DWM_TNP_VISIBLE | DWM_TNP_SOURCECLIENTAREAONLY,
            rcDestination: dest,
            rcSource: RECT { left: inset.0, top: inset.1 + src.1, right: inset.0 + src.0, bottom: inset.1 + src.2 },
            opacity,
            fVisible: (dest.right > dest.left && dest.bottom > dest.top && opacity > 0).into(),
            fSourceClientAreaOnly: false.into(),
        };
        unsafe {
            let _ = DwmUpdateThumbnailProperties(th, &p);
        }
    }

    /// O minimizar de `src` começou: dispara o relógio. false = não era nosso.
    pub fn start(src: HWND) -> bool {
        if !T.with_borrow(|t| t.as_ref().is_some_and(|t| t.src == src)) {
            return false;
        }
        arm();
        let target = crate::minimize_target(src).unwrap_or_else(super::fallback_target);
        T.with_borrow_mut(|t| {
            let Some(t) = t.as_mut() else { return };
            let (ox, oy) = t.origin;
            let f = |v: i32, o: i32| (v - o) as f32;
            let fr = t.frame;
            t.geo = Some(Geo {
                win: [f(fr.left, ox), f(fr.top, oy), f(fr.right, ox), f(fr.bottom, oy)],
                icon: [f(target.left, ox), f(target.top, oy), f(target.right, ox), f(target.bottom, oy)],
                genie: t.thumbs.len() > 1,
            });
            t.start = Some(Instant::now());
        });
        true
    }

    /// Um quadro; true = acabou (tudo desligado).
    pub fn frame() -> bool {
        let done = T.with_borrow_mut(|t| {
            let Some(t) = t.as_mut() else { return true };
            let (Some(start), Some(g)) = (t.start, t.geo) else { return true };
            let tt = start.elapsed().as_secs_f32() / DURATION;
            if tt >= 1.0 {
                return true;
            }
            let fr = t.frame;
            let (fw, fh) = (fr.right - fr.left, (fr.bottom - fr.top) as f32);
            let [wl, wt, wr, wb] = g.win;
            let [il, it, ir, ib] = g.icon;
            let n = t.thumbs.len();
            if n == 1 {
                // Escala: encolhe até um pouco menor que o ícone e some na segunda metade.
                let s = 0.7 * ((ir - il) / (wr - wl)).min((ib - it) / (wb - wt));
                let (cx, cy) = ((il + ir) / 2.0, (it + ib) / 2.0);
                let (hw, hh) = ((wr - wl) * s / 2.0, (wb - wt) * s / 2.0);
                let e = 1.0 - (1.0 - tt).powi(3);
                let r = RECT {
                    left: lerp(wl, cx - hw, e).round() as i32,
                    top: lerp(wt, cy - hh, e).round() as i32,
                    right: lerp(wr, cx + hw, e).round() as i32,
                    bottom: lerp(wb, cy + hh, e).round() as i32,
                };
                let a = 1.0 - smooth((tt - 0.4) / 0.6);
                set(t.thumbs[0], t.inset, (fw, 0, fh as i32), r, (a * 255.0) as u8);
                return false;
            }
            // Gênio: a imagem desce pelo funil; cada faixa pega a largura do funil na altura dela.
            let b = smooth((tt - 0.2) / 0.8);
            let (top, bot) = (lerp(wt, it, b), lerp(wb, ib, b));
            for (i, &th) in t.thumbs.iter().enumerate() {
                let (v0, v1) = (i as f32 / n as f32, (i + 1) as f32 / n as f32);
                let (y0, y1) = (top + v0 * (bot - top), top + v1 * (bot - top));
                let (sy0, sy1) = ((fh * v0).round() as i32, (fh * v1).round() as i32);
                match g.row(tt, (y0 + y1) / 2.0) {
                    Some((_, l, r, a)) => {
                        let dest = RECT { left: l.round() as i32, top: y0.floor() as i32, right: r.round() as i32, bottom: y1.ceil() as i32 };
                        set(th, t.inset, (fw, sy0, sy1), dest, (a.clamp(0.0, 1.0) * 255.0) as u8);
                    }
                    None => set(th, t.inset, (fw, sy0, sy1), RECT::default(), 0),
                }
            }
            false
        });
        if done {
            cancel();
        }
        done
    }

    /// Desliga tudo (fim, ou o minimizar não veio).
    pub fn cancel() {
        let had = T.with_borrow_mut(|t| t.take());
        if let Some(t) = had {
            for th in t.thumbs {
                unsafe {
                    let _ = DwmUnregisterThumbnail(th);
                }
            }
            if let Some(l) = layer() {
                unsafe {
                    let _ = ShowWindow(l, SW_HIDE);
                }
            }
        }
    }
}
