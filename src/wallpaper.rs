//! Plano de fundo animado: um shader (`wallpaper.hlsl`) desenha o estilo escolhido atrás dos
//! ícones da área de trabalho, numa janela por monitor, com Direct3D 11.
//!
//! - Onde: o Explorer cria, a pedido (mensagem 0x052C ao Progman), uma camada de fundo
//!   `WorkerW`. No Windows 11 24H2 e mais novos ela é filha do Progman, abaixo dos ícones; antes
//!   era uma janela solta logo atrás da que tem os ícones. As nossas janelas viram filhas dela.
//!   Só conteúdo de swap chain (DXGI) aparece ali: GDI não é composto no desktop novo.
//! - Quanto: 30 quadros por segundo; cada monitor para de desenhar quando uma janela maximizada
//!   ou em tela cheia o cobre, e tudo para na bateria (opção) ou com a tela desligada.
//! - Thread própria: o laço de desenho não disputa com o dock. Mudanças chegam por
//!   [`WM_WALLPAPER`] (estilo ou opção) e a thread se refaz sozinha se o Explorer reiniciar ou
//!   os monitores mudarem.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{DXGI_STATUS_OCCLUDED, HMODULE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
    CreateDXGIFactory1, DXGI_GPU_PREFERENCE_MINIMUM_POWER, IDXGIAdapter, IDXGIDevice, IDXGIFactory2, IDXGIFactory6, IDXGISwapChain1,
};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromWindow};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, Interface, PCSTR, s, w};

/// Pede à thread que releia o estilo e as opções.
const WM_WALLPAPER: u32 = WM_APP + 90;
/// Quadros por segundo com o fundo visível.
const FPS: u64 = 30;
/// A cada quanto a thread confere janelas por cima, bateria, tema e monitores.
const CHECK: Duration = Duration::from_millis(500);

/// Estilos: chave (configuração), nome (pt-BR, passa por `tr`).
pub const STYLES: [(&str, &str); 3] = [("waves", "Ondas"), ("aurora", "Aurora"), ("silk", "Seda")];

static THREAD: Mutex<Option<(u32, std::thread::JoinHandle<()>)>> = Mutex::new(None);
/// Estilo ligado: índice em [`STYLES`] + 1; 0 = desligado.
static CURRENT: AtomicU32 = AtomicU32::new(0);

/// Liga, troca ou desliga conforme as Configurações. Chamado no início e a cada mudança.
pub fn apply() {
    let style = crate::config::wallpaper().and_then(|k| STYLES.iter().position(|(key, _)| *key == k));
    CURRENT.store(style.map_or(0, |i| i as u32 + 1), Ordering::Release);
    let Ok(mut t) = THREAD.lock() else { return };
    match (style, t.as_ref()) {
        (None, Some(_)) => {
            if let Some((tid, handle)) = t.take() {
                unsafe {
                    let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
                }
                let _ = handle.join();
            }
        }
        (Some(_), Some((tid, _))) => unsafe {
            let _ = PostThreadMessageW(*tid, WM_WALLPAPER, WPARAM(0), LPARAM(0));
        },
        (Some(_), None) => {
            let (tx, rx) = std::sync::mpsc::channel();
            let handle = std::thread::spawn(move || {
                let _ = tx.send(unsafe { GetCurrentThreadId() });
                run();
            });
            if let Ok(tid) = rx.recv() {
                *t = Some((tid, handle));
            }
        }
        (None, None) => {}
    }
}

/// Para a thread (saída do dock): as janelas somem e o fundo do Windows volta.
pub fn shutdown() {
    CURRENT.store(0, Ordering::Release);
    if let Ok(mut t) = THREAD.lock()
        && let Some((tid, handle)) = t.take()
    {
        unsafe {
            let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        let _ = handle.join();
    }
}

/// Segundos para o shader: hora do dia, para o movimento seguir de onde estava ao reabrir.
fn seconds() -> f32 {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    (d.as_secs() % 86_400) as f32 + d.subsec_millis() as f32 / 1000.0
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Params {
    res: [f32; 2],
    time: f32,
    night: f32,
    style: f32,
    warm: f32,
    seed_a: f32,
    seed_b: f32,
    blend: f32,
    pad: [f32; 3],
}

/// A cada quanto o desenho muda (formas e família de cores).
const PERIOD: u64 = 4 * 3600;
/// Duração da passagem de um desenho para o seguinte (s).
const FADE: f32 = 90.0;

/// Luz e desenho do momento.
#[derive(Clone, Copy)]
struct Look {
    night: f32,
    warm: f32,
    seed_a: f32,
    seed_b: f32,
    blend: f32,
}

fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Fase do dia pela hora local (0..24): `night` de 0 (dia) a 1 (noite), com amanhecer das 5h às
/// 7h30 e anoitecer das 17h às 20h30, e `warm`, a luz dourada perto das 6h40 e das 18h40.
fn sky(hour: f32) -> (f32, f32) {
    let night = if !(5.0..20.5).contains(&hour) {
        1.0
    } else if hour < 7.5 {
        1.0 - smooth((hour - 5.0) / 2.5)
    } else if hour < 17.0 {
        0.0
    } else {
        smooth((hour - 17.0) / 3.5)
    };
    let bell = |c: f32, w: f32| (-((hour - c) / w).powi(2)).exp();
    (night, (bell(6.65, 1.1) + bell(18.65, 1.2)).min(1.0))
}

fn local_hour() -> f32 {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    t.wHour as f32 + t.wMinute as f32 / 60.0 + t.wSecond as f32 / 3600.0
}

/// Desenho `idx` (um a cada [`PERIOD`]) como número para o shader.
fn seed(idx: u64) -> f32 {
    (idx % 10_000) as f32
}

/// Luz pela hora (ou pelo tema do Windows, sem o modo dinâmico) e o desenho deste período.
fn look() -> Look {
    let (night, warm) = if crate::config::wallpaper_dynamic() {
        sky(local_hour())
    } else {
        ((!crate::light_theme()) as u32 as f32, 0.0)
    };
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let idx = secs / PERIOD;
    let into = (secs % PERIOD) as f32;
    Look { night, warm, seed_a: seed(idx.wrapping_sub(1)), seed_b: seed(idx), blend: smooth(into / FADE) }
}

fn params(w: u32, h: u32, time: f32, style: usize, l: Look) -> Params {
    Params {
        res: [w as f32, h as f32],
        time,
        night: l.night,
        style: style as f32,
        warm: l.warm,
        seed_a: l.seed_a,
        seed_b: l.seed_b,
        blend: l.blend,
        pad: [0.0; 3],
    }
}

/// Dispositivo, shaders e o buffer de parâmetros, compartilhados por todas as telas.
struct Gpu {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    params: ID3D11Buffer,
}

fn compile(entry: PCSTR, target: PCSTR) -> Option<Vec<u8>> {
    let src = include_str!("wallpaper.hlsl");
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    let ok = unsafe {
        D3DCompile(
            src.as_ptr().cast(),
            src.len(),
            s!("wallpaper.hlsl"),
            None,
            None,
            entry,
            target,
            1 << 15, // D3DCOMPILE_OPTIMIZATION_LEVEL3
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if ok.is_err() {
        if let Some(e) = errors {
            let msg = unsafe { std::slice::from_raw_parts(e.GetBufferPointer() as *const u8, e.GetBufferSize()) };
            let _ = std::fs::write(std::env::temp_dir().join("arcdockwin-wallpaper.log"), msg);
        }
        return None;
    }
    let blob = code?;
    Some(unsafe { std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize()) }.to_vec())
}

/// Shaders compilados uma vez por execução (compilar leva ~1,5 s) e usados por todos.
fn shaders() -> Option<&'static (Vec<u8>, Vec<u8>)> {
    static CODE: OnceLock<Option<(Vec<u8>, Vec<u8>)>> = OnceLock::new();
    CODE.get_or_init(|| Some((compile(s!("vs"), s!("vs_5_0"))?, compile(s!("ps"), s!("ps_5_0"))?))).as_ref()
}

impl Gpu {
    fn new() -> Option<Self> {
        unsafe {
            let mut device = None;
            let mut ctx = None;
            // Placa de menor consumo (a integrada em notebooks com duas): o fundo não acorda a
            // placa dedicada nem gasta bateria com ela.
            let adapter: Option<IDXGIAdapter> = CreateDXGIFactory1::<IDXGIFactory6>()
                .and_then(|f| f.EnumAdapterByGpuPreference(0, DXGI_GPU_PREFERENCE_MINIMUM_POWER))
                .ok();
            let driver = if adapter.is_some() { D3D_DRIVER_TYPE_UNKNOWN } else { D3D_DRIVER_TYPE_HARDWARE };
            D3D11CreateDevice(
                adapter.as_ref(),
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut ctx),
            )
            .ok()?;
            let (device, ctx): (ID3D11Device, ID3D11DeviceContext) = (device?, ctx?);
            let (vs_code, ps_code) = shaders()?;
            let mut vs = None;
            device.CreateVertexShader(vs_code, None, Some(&mut vs)).ok()?;
            let mut ps = None;
            device.CreatePixelShader(ps_code, None, Some(&mut ps)).ok()?;
            let desc = D3D11_BUFFER_DESC {
                ByteWidth: size_of::<Params>() as u32,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            let mut params = None;
            device.CreateBuffer(&desc, None, Some(&mut params)).ok()?;
            Some(Self { device, ctx, vs: vs?, ps: ps?, params: params? })
        }
    }

    /// Desenha um quadro em `rtv` (tamanho `w`×`h`).
    fn draw(&self, rtv: &ID3D11RenderTargetView, w: u32, h: u32, p: Params) {
        unsafe {
            let c = &self.ctx;
            c.UpdateSubresource(&self.params, 0, None, &p as *const Params as *const _, 0, 0);
            c.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            c.RSSetViewports(Some(&[D3D11_VIEWPORT { Width: w as f32, Height: h as f32, MaxDepth: 1.0, ..Default::default() }]));
            c.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            c.VSSetShader(&self.vs, None);
            c.PSSetShader(&self.ps, None);
            c.PSSetConstantBuffers(0, Some(&[Some(self.params.clone())]));
            c.Draw(3, 0);
        }
    }
}

/// Uma tela: janela filha da camada de fundo e a swap chain dela.
struct Surface {
    hwnd: HWND,
    hmon: HMONITOR,
    rect: RECT,
    /// Tamanho dos buffers (menor que a tela; a placa estica).
    size: (u32, u32),
    swap: IDXGISwapChain1,
    rtv: ID3D11RenderTargetView,
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// Camada de fundo atrás dos ícones (cria se preciso).
fn host() -> Option<HWND> {
    unsafe {
        let progman = FindWindowW(w!("Progman"), None).ok()?;
        let mut r = 0usize;
        SendMessageTimeoutW(progman, 0x052C, WPARAM(0xD), LPARAM(1), SMTO_NORMAL, 1000, Some(&mut r));
        // 24H2+: filha do Progman.
        if let Ok(w) = FindWindowExW(Some(progman), None, w!("WorkerW"), None) {
            return Some(w);
        }
        // Antes: a WorkerW solta logo depois da que tem os ícones.
        unsafe extern "system" fn find(h: HWND, lp: LPARAM) -> BOOL {
            unsafe {
                if FindWindowExW(Some(h), None, w!("SHELLDLL_DefView"), None).is_ok()
                    && let Ok(next) = FindWindowExW(None, Some(h), w!("WorkerW"), None)
                {
                    *(lp.0 as *mut HWND) = next;
                    return BOOL(0);
                }
                BOOL(1)
            }
        }
        let mut out = HWND::default();
        let _ = EnumWindows(Some(find), LPARAM(&mut out as *mut HWND as isize));
        (!out.is_invalid()).then_some(out)
    }
}

fn monitors() -> Vec<(HMONITOR, RECT)> {
    unsafe extern "system" fn add(hmon: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if unsafe { GetMonitorInfoW(hmon, &mut mi) }.as_bool() {
            unsafe { &mut *(lp.0 as *mut Vec<(HMONITOR, RECT)>) }.push((hmon, mi.rcMonitor));
        }
        BOOL(1)
    }
    let mut out: Vec<(HMONITOR, RECT)> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(add), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

fn surfaces(gpu: &Gpu, host: HWND) -> Vec<Surface> {
    let mut origin = RECT::default();
    unsafe {
        let _ = GetWindowRect(host, &mut origin);
    }
    let Ok(factory) = (unsafe { gpu.device.cast::<IDXGIDevice>().and_then(|d| d.GetAdapter()).and_then(|a| a.GetParent::<IDXGIFactory2>()) })
    else {
        return Vec::new();
    };
    let hinst = unsafe { GetModuleHandleW(None) }.unwrap_or_default();
    monitors()
        .into_iter()
        .filter_map(|(hmon, rect)| unsafe {
            let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
            // Solta primeiro e filha depois: criar já filha de outro processo falha.
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("ArcDockWinWallpaper"),
                w!(""),
                WS_POPUP,
                0,
                0,
                w,
                h,
                None,
                None,
                Some(hinst.into()),
                None,
            )
            .ok()?;
            SetWindowLongW(hwnd, GWL_STYLE, (WS_CHILD | WS_VISIBLE | WS_DISABLED).0 as i32);
            let _ = SetParent(hwnd, Some(host));
            let _ = SetWindowPos(
                hwnd,
                None,
                rect.left - origin.left,
                rect.top - origin.top,
                w,
                h,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_SHOWWINDOW,
            );
            // Resolução inteira: com a placa esticando um desenho menor as bordas serrilham.
            let size = (w as u32, h as u32);
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: size.0,
                Height: size.1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                ..Default::default()
            };
            let made = (|| {
                let swap = factory.CreateSwapChainForHwnd(&gpu.device, hwnd, &desc, None, None).ok()?;
                let back: ID3D11Texture2D = swap.GetBuffer(0).ok()?;
                let mut rtv = None;
                gpu.device.CreateRenderTargetView(&back, None, Some(&mut rtv)).ok()?;
                Some((swap, rtv?))
            })();
            match made {
                Some((swap, rtv)) => Some(Surface { hwnd, hmon, rect, size, swap, rtv }),
                None => {
                    let _ = DestroyWindow(hwnd);
                    None
                }
            }
        })
        .collect()
}

/// Monitores cobertos por uma janela maximizada ou em tela cheia (não adianta desenhar).
fn covered(list: &[Surface]) -> Vec<bool> {
    struct Scan<'a> {
        list: &'a [Surface],
        out: Vec<bool>,
    }
    unsafe extern "system" fn each(h: HWND, lp: LPARAM) -> BOOL {
        unsafe {
            let scan = &mut *(lp.0 as *mut Scan);
            if !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() {
                return BOOL(1);
            }
            let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
            // Sobreposições (overlay da placa de vídeo, barras): não tapam o fundo de verdade.
            if ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0 | WS_EX_TRANSPARENT.0) != 0 {
                return BOOL(1);
            }
            let mut cloaked = 0u32;
            let _ = DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4);
            if cloaked != 0 {
                return BOOL(1);
            }
            let mut r = RECT::default();
            let _ = GetWindowRect(h, &mut r);
            let zoomed = IsZoomed(h).as_bool();
            let mon = MonitorFromWindow(h, MONITOR_DEFAULTTONULL);
            for (i, s) in scan.list.iter().enumerate() {
                let m = s.rect;
                let full = r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom;
                if (zoomed && mon == s.hmon) || full {
                    scan.out[i] = true;
                }
            }
            BOOL(1)
        }
    }
    let mut scan = Scan { list, out: vec![false; list.len()] };
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut scan as *mut Scan as isize));
    }
    scan.out
}

fn on_battery() -> bool {
    let mut s = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut s) }.is_ok() && s.ACLineStatus == 0
}

fn run() {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: w!("ArcDockWinWallpaper"),
            ..Default::default()
        };
        RegisterClassW(&wc);
    }
    let Some(gpu) = Gpu::new() else { return };
    let frame = Duration::from_millis(1000 / FPS);
    let mut list: Vec<Surface> = Vec::new();
    let mut host_hwnd = HWND::default();
    let mut idle = vec![];
    let mut paused_all = false;
    let mut lk = look();
    let mut next_check = Instant::now();
    let mut msg = MSG::default();
    loop {
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    return;
                }
                if msg.message == WM_WALLPAPER {
                    next_check = Instant::now();
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let style = CURRENT.load(Ordering::Acquire);
        if style == 0 {
            return;
        }
        let now = Instant::now();
        if now >= next_check {
            next_check = now + CHECK;
            // Explorer reiniciou (a camada sumiu) ou os monitores mudaram: refaz as telas.
            let gone = unsafe { !IsWindow(Some(host_hwnd)).as_bool() } || list.iter().any(|s| unsafe { !IsWindow(Some(s.hwnd)).as_bool() });
            let mons = monitors();
            let changed = mons.len() != list.len() || mons.iter().zip(&list).any(|((_, r), s)| *r != s.rect);
            if gone || changed {
                list.clear();
                if let Some(h) = host() {
                    host_hwnd = h;
                    list = surfaces(&gpu, h);
                }
            }
            lk = look();
            idle = covered(&list);
            paused_all = list.is_empty()
                || idle.iter().all(|&c| c)
                || (crate::config::wallpaper_battery_pause() && on_battery());
        }
        if paused_all {
            unsafe {
                let _ = MsgWaitForMultipleObjects(None, false, CHECK.as_millis() as u32, QS_ALLINPUT);
            }
            continue;
        }
        let t = seconds();
        for (i, s) in list.iter().enumerate() {
            if idle.get(i).copied().unwrap_or(false) {
                continue;
            }
            let (w, h) = s.size;
            let p = params(w, h, t, style as usize - 1, lk);
            gpu.draw(&s.rtv, w, h, p);
            let hr = unsafe { s.swap.Present(0, DXGI_PRESENT(0)) };
            if hr == DXGI_STATUS_OCCLUDED {
                // Tela desligada ou bloqueada: espera sem desenhar.
                next_check = Instant::now() + Duration::from_secs(1);
            }
        }
        let spent = now.elapsed();
        if spent < frame {
            unsafe {
                let _ = MsgWaitForMultipleObjects(None, false, (frame - spent).as_millis() as u32, QS_ALLINPUT);
            }
        }
    }
}

/// `arcdockwin.exe --wallpaper-preview <pasta> <segundos> <hora> <desenho>`: grava em BMP cada
/// estilo no instante, hora do dia e desenho pedidos (para conferir sem mexer na área de trabalho).
pub fn preview_if_requested() -> bool {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("--wallpaper-preview") {
        return false;
    }
    let dir = args.next().map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let time = args.next().and_then(|t| t.parse().ok()).unwrap_or(1200.0);
    let hour = args.next().and_then(|t| t.parse().ok()).unwrap_or(12.0);
    let idx: u64 = args.next().and_then(|t| t.parse().ok()).unwrap_or(0);
    let (night, warm) = sky(hour);
    let l = Look { night, warm, seed_a: seed(idx), seed_b: seed(idx), blend: 1.0 };
    let Some(gpu) = Gpu::new() else { return true };
    for (i, (key, _)) in STYLES.iter().enumerate() {
        if let Some(pm) = render_image(&gpu, i, 960, 540, l, time) {
            let _ = std::fs::write(dir.join(format!("{key}.bmp")), bmp(&pm));
        }
    }
    true
}

/// BMP de 24 bits (de baixo para cima, linhas múltiplas de 4 bytes).
fn bmp(pm: &tiny_skia::Pixmap) -> Vec<u8> {
    let (w, h) = (pm.width() as usize, pm.height() as usize);
    let stride = (w * 3 + 3) & !3;
    let size = 54 + stride * h;
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    let data = pm.data();
    for y in (0..h).rev() {
        let start = out.len();
        for x in 0..w {
            let p = &data[(y * w + x) * 4..];
            out.extend_from_slice(&[p[2], p[1], p[0]]);
        }
        out.resize(start + stride, 0);
    }
    out
}

/// Miniaturas prontas e o momento para o qual foram feitas ([`thumbs_key`]).
static THUMBS: Mutex<Option<(u64, Vec<Option<tiny_skia::Pixmap>>)>> = Mutex::new(None);
/// Uma thread de miniaturas por vez.
static THUMBS_BUSY: AtomicBool = AtomicBool::new(false);

/// Momento das miniaturas: desenho de 4 h, meia hora do dia e o modo (horário ou tema). Se
/// mudou, as guardadas estão velhas.
fn thumbs_key() -> u64 {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let dynamic = crate::config::wallpaper_dynamic();
    let half_hour = if dynamic { (local_hour() * 2.0) as u64 } else { (!crate::light_theme()) as u64 };
    (secs / PERIOD) * 1000 + half_hour * 10 + dynamic as u64
}

/// Desenha as miniaturas numa thread, se as guardadas são de outro momento. Chamado ao abrir o
/// dock e a Central: quando a galeria abre, elas já estão prontas (gerar leva ~2 s).
pub fn prewarm() {
    let key = thumbs_key();
    if THUMBS.lock().ok().and_then(|t| t.as_ref().map(|(k, _)| *k)) == Some(key) {
        return;
    }
    if THUMBS_BUSY.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(move || {
        let shots = thumbnails(320, 180);
        if let Ok(mut t) = THUMBS.lock() {
            *t = Some((key, shots));
        }
        THUMBS_BUSY.store(false, Ordering::Release);
    });
}

/// Miniaturas guardadas (vazias se ainda não ficaram prontas).
fn cached_thumbs() -> Vec<Option<tiny_skia::Pixmap>> {
    THUMBS.lock().ok().and_then(|t| t.as_ref().map(|(_, v)| v.clone())).unwrap_or_else(|| vec![None; STYLES.len()])
}

/// Miniatura de cada estilo como está agora (hora e desenho), em BGRA para o painel.
fn thumbnails(w: u32, h: u32) -> Vec<Option<tiny_skia::Pixmap>> {
    let (l, t) = (look(), seconds());
    let Some(gpu) = Gpu::new() else { return vec![None; STYLES.len()] };
    (0..STYLES.len())
        .map(|i| {
            let mut pm = render_image(&gpu, i, w, h, l, t)?;
            pm.data_mut().chunks_exact_mut(4).for_each(|px| px.swap(0, 2));
            Some(pm)
        })
        .collect()
}

const OFF: u32 = 70_000;
const PICK: u32 = 70_001;
const DYNAMIC: u32 = 70_100;
const BATTERY: u32 = 70_101;

/// Escolha do plano de fundo (Central de Controle): galeria com os estilos e as opções. Abre na
/// hora: as miniaturas vêm prontas de [`prewarm`].
pub struct WallpaperPanel;

impl WallpaperPanel {
    pub fn new() -> Self {
        prewarm();
        Self
    }
}

/// Nome do estilo ligado, para o cartão da Central; `None` = desligado.
pub fn current_name() -> Option<&'static str> {
    let key = crate::config::wallpaper()?;
    STYLES.iter().find(|(k, _)| *k == key).map(|(_, name)| *name)
}

/// Liga (o último estilo usado) ou desliga, pelo círculo do cartão.
pub fn toggle() {
    if crate::config::wallpaper().is_some() {
        crate::config::set_wallpaper(None);
    } else {
        let last = crate::config::wallpaper_last();
        let key = STYLES.iter().find(|(k, _)| Some(*k) == last.as_deref()).map_or(STYLES[0].0, |(k, _)| *k);
        crate::config::set_wallpaper(Some(key));
    }
    apply();
}

impl crate::panel::PanelContent for WallpaperPanel {
    fn width(&self) -> f32 {
        360.0
    }

    fn build(&mut self) -> Vec<crate::panel::Widget> {
        use crate::panel::{Shot, Widget};
        let current = crate::config::wallpaper();
        let thumbs = cached_thumbs();
        let mut shots: Vec<Shot> = STYLES
            .iter()
            .enumerate()
            .map(|(i, (key, name))| Shot {
                id: PICK + i as u32,
                image: thumbs.get(i).cloned().flatten(),
                label: (*name).into(),
                selected: current.as_deref() == Some(*key),
            })
            .collect();
        shots.push(Shot { id: OFF, image: None, label: "Desligado".into(), selected: current.is_none() });
        vec![
            Widget::Label("Plano de fundo animado".into()),
            Widget::Gallery(shots),
            Widget::Toggle { id: DYNAMIC, text: "Muda com o horário do dia".into(), on: crate::config::wallpaper_dynamic(), icon: None },
            Widget::Toggle { id: BATTERY, text: "Pausar na bateria".into(), on: crate::config::wallpaper_battery_pause(), icon: None },
            Widget::Text("Um desenho novo a cada 4 horas. Para quando uma janela cobre a tela.".into()),
        ]
    }

    fn event(&mut self, ev: crate::panel::Event) -> bool {
        use crate::panel::Event;
        match ev {
            Event::Click(OFF) => crate::config::set_wallpaper(None),
            Event::Click(id) if (PICK..PICK + STYLES.len() as u32).contains(&id) => {
                crate::config::set_wallpaper(Some(STYLES[(id - PICK) as usize].0));
            }
            Event::Toggle(DYNAMIC, on) => {
                crate::config::set_wallpaper_dynamic(on);
                prewarm();
            }
            Event::Toggle(BATTERY, on) => crate::config::set_wallpaper_battery_pause(on),
            _ => return false,
        }
        apply();
        true
    }
}

fn render_image(gpu: &Gpu, i: usize, w: u32, h: u32, l: Look, time: f32) -> Option<tiny_skia::Pixmap> {
    unsafe {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            ..Default::default()
        };
        let mut tex = None;
        gpu.device.CreateTexture2D(&desc, None, Some(&mut tex)).ok()?;
        let tex: ID3D11Texture2D = tex?;
        let mut rtv = None;
        gpu.device.CreateRenderTargetView(&tex, None, Some(&mut rtv)).ok()?;
        let p = params(w, h, time, i, l);
        gpu.draw(&rtv?, w, h, p);
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            ..desc
        };
        let mut staging = None;
        gpu.device.CreateTexture2D(&staging_desc, None, Some(&mut staging)).ok()?;
        let staging: ID3D11Texture2D = staging?;
        gpu.ctx.CopyResource(&staging, &tex);
        let mut map = D3D11_MAPPED_SUBRESOURCE::default();
        gpu.ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut map)).ok()?;
        let mut pm = tiny_skia::Pixmap::new(w, h)?;
        let data = pm.data_mut();
        for y in 0..h as usize {
            let row = std::slice::from_raw_parts((map.pData as *const u8).add(y * map.RowPitch as usize), w as usize * 4);
            for x in 0..w as usize {
                let (s, d) = (&row[x * 4..x * 4 + 4], &mut data[(y * w as usize + x) * 4..(y * w as usize + x) * 4 + 4]);
                d.copy_from_slice(&[s[2], s[1], s[0], 255]);
            }
        }
        gpu.ctx.Unmap(&staging, 0);
        Some(pm)
    }
}
