#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod apps;
mod glass;
mod render;
mod taskbar;

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use tiny_skia::Pixmap;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, GetMonitorInfoW, HDC, HGDIOBJ, MONITOR_DEFAULTTONEAREST,
    MONITORINFO, MonitorFromWindow, SelectObject,
};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows::core::{HSTRING, PCWSTR, w};

use apps::App;
use render::{Metrics, Theme};

const TIMER_REFRESH: usize = 2;
const TIMER_EDGE: usize = 3;
/// Quanto tempo o dock continua visível depois que o mouse sai dele.
const HIDE_DELAY: Duration = Duration::from_millis(400);
/// Tempo de subida e descida do dock ao aparecer/sumir (segundos).
const SHOW_TIME: f32 = 0.28;
const HIDE_TIME: f32 = 0.24;
/// Constantes de tempo (segundos) da suavização: entrada/saída do zoom e seguir o mouse.
const HOVER_TAU: f32 = 0.085;
const FOLLOW_TAU: f32 = 0.045;
const MENU_QUIT: usize = 1;
const MENU_PIN: usize = 2;
const MENU_CLOSE: usize = 3;
/// Itens das janelas do app: MENU_WINDOW + índice.
const MENU_WINDOW: usize = 100;
const WM_MOUSELEAVE: u32 = 0x02A3;

thread_local! {
    static DOCK: RefCell<Option<Dock>> = const { RefCell::new(None) };
}

struct Dock {
    hwnd: HWND,
    /// Janela atrás dos ícones que só fornece o desfoque (vidro).
    glass: HWND,
    /// Último retângulo aplicado ao vidro (com o deslocamento vertical), para não repetir.
    glass_rect: Option<(i32, i32, i32, i32)>,
    glass_fx: Option<glass::Glass>,
    m: Metrics,
    theme: Theme,
    icon_size: i32,
    apps: Vec<App>,
    pixmap: Pixmap,
    memdc: HDC,
    bits: *mut u8,
    pos: POINT,
    /// Posição x usada no zoom (segue o mouse suavemente até `mx_target`).
    mx: f32,
    mx_target: f32,
    hover: f32,
    target: f32,
    animating: bool,
    last_frame: Instant,
    tracking: bool,
    /// 0 = visível, 1 = escondido abaixo da tela.
    slide: f32,
    slide_target: f32,
    slide_from: f32,
    slide_start: Instant,
    /// Janelas do dock mostradas (escondidas de vez quando slide chega a 1).
    shown: bool,
    /// Se a janela em foco cobre a tela principal (maximizada ou tela cheia).
    covers: bool,
    /// Última vez que o mouse estava no dock ou na borda de baixo.
    last_inside: Option<Instant>,
}

impl Dock {
    unsafe fn new(hwnd: HWND, glass: HWND) -> Self {
        unsafe {
            let m = Metrics::new(GetDpiForWindow(hwnd) as f32 / 96.0);
            // A barra do Windows fica escondida: o dock ocupa a base da tela.
            let (w, screen_h) = taskbar::screen_size();
            let h = m.window_height();

            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let dib = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
                .expect("CreateDIBSection");
            let memdc = CreateCompatibleDC(None);
            SelectObject(memdc, HGDIOBJ(dib.0));

            let icon_size = m.max.ceil() as i32;
            Self {
                hwnd,
                glass,
                glass_rect: None,
                glass_fx: glass::Glass::new(glass, m.radius).ok(),
                apps: apps::load_pinned(icon_size),
                icon_size,
                m,
                theme: Theme::new(light_theme(), accent_color()),
                pixmap: Pixmap::new(w as u32, h as u32).expect("pixmap"),
                memdc,
                bits: bits as *mut u8,
                pos: POINT { x: 0, y: screen_h - h },
                mx: 0.0,
                mx_target: 0.0,
                hover: 0.0,
                target: 0.0,
                animating: false,
                last_frame: Instant::now(),
                tracking: false,
                slide: 0.0,
                slide_target: 0.0,
                slide_from: 0.0,
                slide_start: Instant::now(),
                shown: true,
                covers: false,
                last_inside: None,
            }
        }
    }

    /// Índice do último fixado, se houver apps abertos não fixados depois dele.
    fn sep_after(&self) -> Option<usize> {
        let pinned = self.apps.iter().filter(|a| a.pinned).count();
        (pinned > 0 && pinned < self.apps.len()).then(|| pinned - 1)
    }

    fn slots(&self) -> Vec<render::Slot> {
        let w = self.pixmap.width() as f32;
        render::layout(self.apps.len(), self.sep_after(), w, &self.m, self.mx, self.hover)
    }

    fn refresh(&mut self) -> bool {
        apps::refresh(&mut self.apps, self.icon_size)
    }

    /// Deslocamento vertical atual da animação de esconder.
    fn offset(&self) -> i32 {
        (self.slide * self.pixmap.height() as f32).round() as i32
    }

    fn redraw(&mut self) {
        if !self.shown {
            return;
        }
        let slots = self.slots();
        let sep = self.sep_after();
        // App em foco: dono da última janela em primeiro plano fora do dock (o menu do dock
        // toma o foco, mas o destaque continua no app).
        let fg = unsafe { GetForegroundWindow() };
        if fg != self.hwnd && fg != self.glass {
            LAST_FG.store(fg.0 as isize, Ordering::Relaxed);
        }
        let fg = HWND(LAST_FG.load(Ordering::Relaxed) as *mut c_void);
        let focus = self.apps.iter().position(|a| a.windows.contains(&fg));
        render::draw(&mut self.pixmap, &slots, sep, &self.apps, &self.m, &self.theme, self.hover, focus);
        self.update_glass(render::box_rect(&slots, &self.m, self.pixmap.height() as f32));

        // RGBA (tiny-skia) -> BGRA (GDI), ambos com alfa pré-multiplicado.
        let src = self.pixmap.data();
        let dst = unsafe { std::slice::from_raw_parts_mut(self.bits, src.len()) };
        for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
        }

        let size = SIZE { cx: self.pixmap.width() as i32, cy: self.pixmap.height() as i32 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let pos = POINT { x: self.pos.x, y: self.pos.y + self.offset() };
        unsafe {
            let _ = UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&pos),
                Some(&size),
                Some(self.memdc),
                Some(&POINT::default()),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }

    /// Move a janela de vidro para trás do fundo do dock.
    fn update_glass(&mut self, rect: Option<(i32, i32, i32, i32)>) {
        let off = self.offset();
        let key = rect.map(|(l, t, r, b)| (l, t + off, r, b + off));
        if key == self.glass_rect {
            return;
        }
        self.glass_rect = key;
        let Some((l, t, r, b)) = rect else {
            unsafe {
                let _ = ShowWindow(self.glass, SW_HIDE);
            }
            return;
        };
        let (w, h) = (r - l, b - t);
        if let Some(fx) = &self.glass_fx {
            fx.resize(w as f32, h as f32);
        }
        unsafe {
            let _ = SetWindowPos(
                self.glass,
                None,
                self.pos.x + l,
                self.pos.y + t + off,
                w,
                h,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW,
            );
        }
    }

    /// Liga o laço de animação (sincronizado com a tela em `main`).
    fn start_anim(&mut self) {
        if !self.animating {
            self.animating = true;
            self.last_frame = Instant::now();
        }
    }

    /// Um quadro de animação, com passo proporcional ao tempo real (independe da taxa da tela).
    fn step(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32().min(0.05);
        self.last_frame = now;

        self.hover = smooth(self.hover, self.target, dt, HOVER_TAU, 0.002);
        self.mx = smooth(self.mx, self.mx_target, dt, FOLLOW_TAU, 0.25);

        // Subir/descer: curva com duração fixa; recomeça do ponto atual se o alvo mudar no meio.
        let (dur, ease): (f32, fn(f32) -> f32) =
            if self.slide_target < self.slide_from { (SHOW_TIME, ease_out_cubic) } else { (HIDE_TIME, ease_in_out_cubic) };
        let t = ((now - self.slide_start).as_secs_f32() / dur).min(1.0);
        self.slide = self.slide_from + (self.slide_target - self.slide_from) * ease(t);

        if self.hover == self.target && self.mx == self.mx_target && t >= 1.0 {
            self.animating = false;
        }
        self.apply_shown();
        self.redraw();
    }

    /// Esconde as janelas quando o dock termina de descer e mostra de novo ao subir.
    fn apply_shown(&mut self) {
        let want = self.slide < 1.0;
        if want == self.shown {
            return;
        }
        self.shown = want;
        self.glass_rect = None;
        unsafe {
            if want {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            } else {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
                let _ = ShowWindow(self.glass, SW_HIDE);
            }
        }
    }

    /// Mouse encostado na borda de baixo da tela, na largura do dock.
    fn at_edge(&self) -> bool {
        let mut pt = POINT::default();
        if unsafe { GetCursorPos(&mut pt) }.is_err() {
            return false;
        }
        let (_, sh) = taskbar::screen_size();
        if pt.y < sh - 2 || pt.y >= sh {
            return false;
        }
        render::box_rect(&self.slots(), &self.m, self.pixmap.height() as f32)
            .is_some_and(|(l, _, r, _)| pt.x >= self.pos.x + l && pt.x < self.pos.x + r)
    }

    /// Decide se o dock deve aparecer ou sumir (chamado a cada ~50 ms).
    fn edge_tick(&mut self) {
        if let Some(c) = foreground_covers(&[self.hwnd, self.glass]) {
            self.covers = c;
        }
        let now = Instant::now();
        if self.tracking || MENU_OPEN.load(Ordering::Relaxed) || self.at_edge() {
            self.last_inside = Some(now);
        }
        let near = self.last_inside.is_some_and(|t| now - t < HIDE_DELAY);
        let target = if !self.covers || near { 0.0 } else { 1.0 };
        if target != self.slide_target {
            self.slide_from = self.slide;
            self.slide_start = now;
            self.slide_target = target;
            if target == 1.0 {
                self.target = 0.0;
            }
            self.start_anim();
        }
    }

    fn hit(&self, x: f32) -> Option<usize> {
        self.slots()
            .iter()
            .position(|s| x >= s.x - self.m.gap / 2.0 && x < s.x + s.size + self.m.gap / 2.0)
    }
}

/// Aproxima `v` de `target` exponencialmente, com constante de tempo `tau` (segundos).
fn smooth(v: f32, target: f32, dt: f32, tau: f32, eps: f32) -> f32 {
    let n = v + (target - v) * (1.0 - (-dt / tau).exp());
    if (target - n).abs() < eps { target } else { n }
}

fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

fn ease_in_out_cubic(t: f32) -> f32 {
    if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 }
}

/// A janela em foco ocupa a tela principal inteira (maximizada ou tela cheia)?
/// `None` quando o foco está no próprio dock (mantém o estado anterior).
fn foreground_covers(own: &[HWND]) -> Option<bool> {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() {
            return Some(false);
        }
        if own.contains(&fg) {
            return None;
        }
        let mut buf = [0u16; 64];
        let n = GetClassNameW(fg, &mut buf).max(0) as usize;
        let class = String::from_utf16_lossy(&buf[..n]);
        // Área de trabalho e barra do Windows.
        if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
            return Some(false);
        }
        if IsIconic(fg).as_bool() {
            return Some(false);
        }
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let mon = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
        if !GetMonitorInfoW(mon, &mut mi).as_bool() || mi.dwFlags & MONITORINFOF_PRIMARY == 0 {
            return Some(false);
        }
        if IsZoomed(fg).as_bool() {
            return Some(true);
        }
        let mut r = RECT::default();
        let _ = GetWindowRect(fg, &mut r);
        let m = mi.rcMonitor;
        Some(r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom)
    }
}

fn with_dock<R>(f: impl FnOnce(&mut Dock) -> R) -> Option<R> {
    // try_borrow_mut: chamadas do Windows podem reentrar no wndproc.
    DOCK.with(|d| d.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

fn mouse_x(lp: LPARAM) -> f32 {
    (lp.0 & 0xffff) as i16 as f32
}

/// Tema da barra de tarefas (claro/escuro).
fn light_theme() -> bool {
    let mut value: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut _ as *mut c_void),
            Some(&mut size),
        )
    };
    ok.is_ok() && value != 0
}

/// Cor de destaque do Windows (no registro vem como ABGR).
fn accent_color() -> Option<[u8; 4]> {
    let mut value: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\DWM"),
            w!("AccentColor"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut _ as *mut c_void),
            Some(&mut size),
        )
    };
    ok.is_ok().then(|| [value as u8, (value >> 8) as u8, (value >> 16) as u8, 255])
}

/// Janela em primeiro plano mudou: atualiza o destaque do app em foco na hora.
unsafe extern "system" fn on_foreground(
    _: HWINEVENTHOOK,
    _: u32,
    hwnd: HWND,
    _: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    with_dock(|d| {
        if hwnd == d.hwnd || hwnd == d.glass {
            return;
        }
        // Janela ainda não listada (acabou de abrir): atualiza os apps antes.
        if !hwnd.is_invalid() && !d.apps.iter().any(|a| a.windows.contains(&hwnd)) {
            d.refresh();
        }
        d.redraw();
    });
}

/// Menu do botão direito: com `target` (key, fixado, janelas), as opções do app; sempre "Sair".
unsafe fn context_menu(hwnd: HWND, target: Option<(String, bool, Vec<HWND>)>) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let fg = GetForegroundWindow();
        if let Some((_, pinned, windows)) = &target {
            if windows.len() >= 2 {
                for (k, &win) in windows.iter().enumerate() {
                    let flags = if win == fg { MF_STRING | MF_CHECKED } else { MF_STRING };
                    let title = HSTRING::from(apps::menu_title(win));
                    let _ = AppendMenuW(menu, flags, MENU_WINDOW + k, &title);
                }
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            }
            let pin = if *pinned { w!("Desafixar do dock") } else { w!("Fixar no dock") };
            let _ = AppendMenuW(menu, MF_STRING, MENU_PIN, pin);
            match windows.len() {
                0 => {}
                1 => {
                    let _ = AppendMenuW(menu, MF_STRING, MENU_CLOSE, w!("Fechar janela"));
                }
                n => {
                    let text = HSTRING::from(format!("Fechar {n} janelas"));
                    let _ = AppendMenuW(menu, MF_STRING, MENU_CLOSE, &text);
                }
            }
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        }
        let _ = AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("Sair do ArcDockWin"));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        MENU_OPEN.store(true, Ordering::Relaxed);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            pt.x,
            pt.y,
            None,
            hwnd,
            None,
        );
        MENU_OPEN.store(false, Ordering::Relaxed);
        let _ = DestroyMenu(menu);

        let cmd = cmd.0 as usize;
        if cmd == MENU_QUIT {
            let _ = DestroyWindow(hwnd);
            return;
        }
        let Some((key, pinned, windows)) = target else { return };
        match cmd {
            MENU_PIN => {
                with_dock(|d| apps::set_pinned(&mut d.apps, &key, !pinned));
            }
            MENU_CLOSE => {
                for &win in &windows {
                    let _ = PostMessageW(Some(win), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
            c if c >= MENU_WINDOW => {
                if let Some(&win) = windows.get(c - MENU_WINDOW) {
                    apps::focus_window(win);
                }
            }
            _ => return,
        }
        with_dock(|d| {
            d.refresh();
            d.redraw();
        });
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_MOUSEMOVE => {
                with_dock(|d| {
                    d.mx_target = mouse_x(lp);
                    // Entrando no dock: o zoom já nasce sob o mouse, sem deslizar de onde estava.
                    if d.hover < 0.05 {
                        d.mx = d.mx_target;
                    }
                    if !d.tracking {
                        d.tracking = true;
                        let mut tme = TRACKMOUSEEVENT {
                            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        let _ = TrackMouseEvent(&mut tme);
                    }
                    d.target = 1.0;
                    d.start_anim();
                });
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                with_dock(|d| {
                    d.tracking = false;
                    d.target = 0.0;
                    d.start_anim();
                });
                LRESULT(0)
            }
            WM_TIMER => {
                with_dock(|d| match wp.0 {
                    TIMER_EDGE => d.edge_tick(),
                    TIMER_REFRESH => {
                        taskbar::ensure_hidden();
                        if d.refresh() {
                            d.redraw();
                        }
                    }
                    _ => {}
                });
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let target = with_dock(|d| {
                    if d.refresh() {
                        d.redraw();
                    }
                    d.hit(mouse_x(lp)).map(|i| (d.apps[i].source.clone(), d.apps[i].windows.clone()))
                })
                .flatten();
                if let Some((source, windows)) = target {
                    apps::activate(&source, &windows);
                }
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                // App sob o mouse; fora de um ícone ou no botão Iniciar, só "Sair".
                let target = with_dock(|d| {
                    if d.refresh() {
                        d.redraw();
                    }
                    d.hit(mouse_x(lp))
                        .map(|i| &d.apps[i])
                        .filter(|a| a.source != apps::START)
                        .map(|a| (a.key.clone(), a.pinned, a.windows.clone()))
                })
                .flatten();
                context_menu(hwnd, target);
                LRESULT(0)
            }
            WM_ENDSESSION => {
                if wp.0 != 0 {
                    taskbar::restore();
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                taskbar::restore();
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ if msg == TASKBAR_CREATED.load(Ordering::Relaxed) && msg != 0 => {
                // O Explorer reiniciou e recriou a barra: esconde de novo.
                taskbar::hide();
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

/// Menu do botão direito aberto: o dock não some enquanto ele estiver na tela.
static MENU_OPEN: AtomicBool = AtomicBool::new(false);

/// Última janela em primeiro plano fora do dock (HWND como número).
static LAST_FG: AtomicIsize = AtomicIsize::new(0);

unsafe fn register(hinst: HINSTANCE, name: PCWSTR, proc: WNDPROC) -> windows::core::Result<()> {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: proc,
            hInstance: hinst,
            lpszClassName: name,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        Ok(())
    }
}

unsafe extern "system" fn glass_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

use windows::Win32::Foundation::HINSTANCE;

fn main() -> windows::core::Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        // Uma cópia só.
        let _single = windows::Win32::System::Threading::CreateMutexW(None, true, w!("ArcDockWin.Single"))?;
        if windows::Win32::Foundation::GetLastError() == windows::Win32::Foundation::ERROR_ALREADY_EXISTS {
            return Ok(());
        }

        let hinst: HINSTANCE = GetModuleHandleW(None)?.into();
        let ex = WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE;

        // Vidro: sem superfície GDI, o conteúdo vem só da composição (glass.rs).
        let glass_class = w!("ArcDockWinGlass");
        register(hinst, glass_class, Some(glass_proc))?;
        let glass = CreateWindowExW(
            ex | WS_EX_NOREDIRECTIONBITMAP,
            glass_class,
            glass_class,
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst),
            None,
        )?;

        // Ícones: janela em camadas, "dona" pelo vidro para ficar sempre à frente dele.
        let class = w!("ArcDockWin");
        register(hinst, class, Some(wndproc))?;
        let hwnd = CreateWindowExW(
            ex | WS_EX_LAYERED,
            class,
            class,
            WS_POPUP,
            0,
            0,
            0,
            0,
            Some(glass),
            None,
            Some(hinst),
            None,
        )?;

        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Relaxed);
        taskbar::hide();

        let mut dock = Dock::new(hwnd, glass);
        dock.refresh();
        dock.redraw();
        DOCK.with(|d| *d.borrow_mut() = Some(dock));

        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), TIMER_REFRESH, 1000, None);
        SetTimer(Some(hwnd), TIMER_EDGE, 50, None);
        let _ = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(on_foreground),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        );

        // Parado: espera mensagens. Animando: um quadro por atualização da tela (DwmFlush
        // espera o próximo quadro do compositor), na taxa do monitor (60, 120, 144 Hz...).
        let mut msg = MSG::default();
        loop {
            if with_dock(|d| d.animating).unwrap_or(false) {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        return Ok(());
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                with_dock(|d| if d.animating { d.step() });
                let _ = DwmFlush();
            } else {
                if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    Ok(())
}
