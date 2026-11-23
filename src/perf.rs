//! Painel "Desempenho" (clique no CPU/RAM da topbar): uso de CPU, memória e GPU e as
//! temperaturas de CPU e GPU. Tudo é lido numa thread que só existe com o painel aberto: ela
//! abre PDH, DXGI, ADL (AMD) e NVML (NVIDIA) ao começar, relê a cada segundo, avisa a topbar
//! com `control::WM_UPDATE` e solta tudo quando o painel fecha. Fonte que falha vira "—".

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Win32::Foundation::{FreeLibrary, HMODULE, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PdhAddEnglishCounterW, PdhCloseQuery,
    PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SW_SHOWNORMAL};
use windows::core::{PCSTR, PCWSTR, w};

use crate::panel::{self, Event, PanelContent, Widget};

const TASKMGR: u32 = 1;

// Glifos da Segoe Fluent Icons.
const GLYPH_RAM: u16 = 0xE964;
const GLYPH_GPU: u16 = 0xE7F4;
const GLYPH_TEMP: u16 = 0xE9CA;
const GLYPH_ACTIVITY: u16 = 0xE9D9;
const INTERVAL: Duration = Duration::from_secs(1);

/// Uma leitura; `None` = indisponível.
#[derive(Default, Clone)]
struct Sample {
    cpu: Option<u8>,
    /// Clock atual da CPU em MHz (só pelo ADL).
    clock: Option<f64>,
    gpu_name: Option<String>,
    /// (usado, total) em bytes.
    mem: Option<(u64, u64)>,
    gpu: Option<f64>,
    vram: Option<(u64, Option<u64>)>,
    cpu_temp: Option<f64>,
    gpu_temp: Option<f64>,
}

pub struct PerfPanel {
    sample: Arc<Mutex<Sample>>,
    stop: Arc<AtomicBool>,
    worker: std::thread::Thread,
    cpu_name: String,
}

impl PerfPanel {
    pub fn new() -> Self {
        let (sample, stop) = (Arc::new(Mutex::new(Sample::default())), Arc::new(AtomicBool::new(false)));
        let (s, st) = (sample.clone(), stop.clone());
        let hwnd = crate::topbar::hwnd().0 as isize;
        let worker = std::thread::spawn(move || collect(s, st, hwnd)).thread().clone();
        Self { sample, stop, worker, cpu_name: cpu_name().unwrap_or_else(|| "Em uso".into()) }
    }
}

impl Drop for PerfPanel {
    fn drop(&mut self) {
        // A thread acorda, vê o pedido e solta PDH/ADL/NVML sozinha.
        self.stop.store(true, Ordering::Release);
        self.worker.unpark();
    }
}

/// Tamanho curto: "446M" ou "7.8G".
fn size(b: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let m = b as f64 / MB;
    if m >= 1024.0 { format!("{:.1}G", m / 1024.0) } else { format!("{m:.0}M") }
}

/// Nível de alerta da barra: 0 normal, 1 alto, 2 crítico.
fn level(v: Option<f64>, high: f64, critical: f64) -> u8 {
    match v {
        Some(v) if v >= critical => 2,
        Some(v) if v >= high => 1,
        _ => 0,
    }
}

/// Temperatura numa barra de 0 a 100 °C.
fn temp(label: &str, t: Option<f64>) -> Widget {
    let lvl = level(t, 80.0, 90.0);
    let detail = match (t, lvl) {
        (None, _) => "Sem sensor",
        (_, 0) => "Normal",
        (_, 1) => "Quente",
        _ => "Muito quente",
    };
    Widget::Meter {
        glyph: GLYPH_TEMP,
        label: label.into(),
        value: t.map(|t| format!("{t:.0} °C")).unwrap_or_else(|| "—".into()),
        detail: Some(detail.into()),
        frac: t.map(|t| (t / 100.0) as f32),
        level: lvl,
    }
}

/// "12%" ou "—".
fn pct(v: Option<f64>) -> String {
    v.map(|v| format!("{v:.0}%")).unwrap_or_else(|| "—".into())
}

impl PanelContent for PerfPanel {
    fn build(&mut self) -> Vec<Widget> {
        let s = self.sample.lock().unwrap().clone();
        const GB: f64 = 1024.0 * 1024.0 * 1024.0;
        let cpu = s.cpu.map(f64::from);
        let mem = s.mem.filter(|m| m.1 > 0);
        let mem_pct = mem.map(|(u, t)| u as f64 * 100.0 / t as f64);
        // Memória da placa primeiro: o nome longo é que leva as reticências.
        let gpu_detail = [
            s.vram.map(|(u, t)| t.map(|t| format!("{} / {}", size(u), size(t))).unwrap_or_else(|| size(u))),
            s.gpu_name.clone(),
        ];
        let gpu_detail = gpu_detail.into_iter().flatten().collect::<Vec<_>>().join(" · ");
        let meter = |glyph, label: &str, v: Option<f64>, detail: Option<String>| Widget::Meter {
            glyph,
            label: label.into(),
            value: pct(v),
            detail,
            frac: v.map(|v| (v / 100.0) as f32),
            level: level(v, 75.0, 90.0),
        };
        vec![
            Widget::Banner { glyph: crate::topbar::GLYPH_CPU, image: None, text: "Desempenho".into(), sub: self.cpu_name.clone() },
            Widget::Separator,
            Widget::Label("Recursos".into()),
            meter(crate::topbar::GLYPH_CPU, "CPU", cpu, s.clock.map(|c| format!("{:.2} GHz", c / 1000.0))),
            meter(GLYPH_RAM, "Memória", mem_pct, mem.map(|(u, t)| format!("{:.1} / {:.1} GB", u as f64 / GB, t as f64 / GB))),
            meter(GLYPH_GPU, "GPU", s.gpu, (!gpu_detail.is_empty()).then_some(gpu_detail)),
            Widget::Label("Temperaturas".into()),
            temp("CPU", s.cpu_temp),
            temp("GPU", s.gpu_temp),
            Widget::Separator,
            Widget::Row { id: TASKMGR, glyph: Some(GLYPH_ACTIVITY), text: "Abrir Gerenciador de Tarefas".into(), sub: None, selected: false },
        ]
    }

    fn event(&mut self, ev: Event) -> bool {
        if let Event::Click(TASKMGR) = ev {
            unsafe { ShellExecuteW(None, w!("open"), w!("taskmgr.exe"), None, None, SW_SHOWNORMAL) };
            panel::close();
        }
        false
    }

    fn notify(&mut self, msg: u32, _: usize) -> bool {
        msg == crate::control::WM_UPDATE
    }
}

/// Nome do processador no registro, sem os espaços de enchimento.
fn cpu_name() -> Option<String> {
    let mut buf = [0u16; 128];
    let mut len = (buf.len() * 2) as u32;
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!("HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0"),
            w!("ProcessorNameString"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as _),
            Some(&mut len),
        )
        .ok()
        .ok()?;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let s = String::from_utf16_lossy(&buf[..n]).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Thread de leitura: abre as fontes, lê até `stop` e solta tudo ao sair (pelos `Drop`).
fn collect(sample: Arc<Mutex<Sample>>, stop: Arc<AtomicBool>, hwnd: isize) {
    let gpu = Gpu::pick();
    let pdh = Pdh::open();
    let adl = Adl::open();
    // NVML só com placa NVIDIA escolhida.
    let nvml = gpu.as_ref().filter(|g| g.vendor == 0x10DE).and_then(|_| Nvml::open());
    let mut usage = crate::status::Usage::new();
    let luid = gpu.as_ref().map(|g| g.luid.clone()).unwrap_or_default();
    let mut first = true;
    while !stop.load(Ordering::Acquire) {
        let mut s = Sample { cpu: usage.cpu(), mem: memory(), ..Default::default() };
        if let Some(p) = &pdh {
            let (util, used) = p.read(&luid);
            s.gpu = util;
            s.vram = used.map(|u| (u, gpu.as_ref().map(|g| g.dedicated).filter(|&t| t > 0)));
        }
        s.gpu_name = gpu.as_ref().map(|g| g.name.clone());
        if let Some(a) = &adl {
            (s.cpu_temp, s.gpu_temp, s.clock) = a.read();
            // Placa escolhida não é AMD: a temperatura de GPU do ADL seria de outra.
            if gpu.as_ref().is_some_and(|g| g.vendor != 0x1002) {
                s.gpu_temp = None;
            }
        }
        if let Some(n) = &nvml {
            s.gpu_temp = n.read();
        }
        *sample.lock().unwrap() = s;
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as _)), crate::control::WM_UPDATE, WPARAM(0), LPARAM(0));
        }
        // A primeira volta é curta: CPU e PDH precisam de duas leituras para dar a taxa.
        std::thread::park_timeout(if first { Duration::from_millis(300) } else { INTERVAL });
        first = false;
    }
}

fn memory() -> Option<(u64, u64)> {
    let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    unsafe { GlobalMemoryStatusEx(&mut m) }.ok()?;
    Some((m.ullTotalPhys - m.ullAvailPhys, m.ullTotalPhys))
}

/// GPU principal pelo DXGI: a de mais memória dedicada (sem o renderizador de software).
struct Gpu {
    /// Prefixo das instâncias PDH dessa placa ("luid_0x…_0x…", minúsculo).
    luid: String,
    dedicated: u64,
    vendor: u32,
    name: String,
}

impl Gpu {
    fn pick() -> Option<Self> {
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
        (0..)
            .map_while(|i| unsafe { factory.EnumAdapters1(i) }.ok())
            .filter_map(|a| unsafe { a.GetDesc1() }.ok())
            .filter(|d| d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0 && d.VendorId != 0x1414)
            .max_by_key(|d| d.DedicatedVideoMemory)
            .map(|d| Gpu {
                luid: format!("luid_0x{:08x}_0x{:08x}", d.AdapterLuid.HighPart as u32, d.AdapterLuid.LowPart),
                dedicated: d.DedicatedVideoMemory as u64,
                vendor: d.VendorId,
                name: {
                    let n = d.Description.iter().position(|&c| c == 0).unwrap_or(d.Description.len());
                    String::from_utf16_lossy(&d.Description[..n]).trim().to_string()
                },
            })
    }
}

/// Contadores de GPU do Windows (os mesmos do Gerenciador de Tarefas).
struct Pdh {
    query: PDH_HQUERY,
    engine: PDH_HCOUNTER,
    memory: PDH_HCOUNTER,
}

impl Pdh {
    fn open() -> Option<Self> {
        let mut query = PDH_HQUERY::default();
        unsafe {
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 {
                return None;
            }
            // Contador que falha fica nulo e só devolve lista vazia.
            let (mut engine, mut memory) = (PDH_HCOUNTER::default(), PDH_HCOUNTER::default());
            PdhAddEnglishCounterW(query, w!("\\GPU Engine(*)\\Utilization Percentage"), 0, &mut engine);
            PdhAddEnglishCounterW(query, w!("\\GPU Adapter Memory(*)\\Dedicated Usage"), 0, &mut memory);
            PdhCollectQueryData(query);
            Some(Self { query, engine, memory })
        }
    }

    /// Valores de todas as instâncias de um contador (nomes em minúsculas).
    fn values(c: PDH_HCOUNTER) -> Vec<(String, f64)> {
        let (mut size, mut count) = (0u32, 0u32);
        unsafe {
            PdhGetFormattedCounterArrayW(c, PDH_FMT_DOUBLE, &mut size, &mut count, None);
            if size == 0 {
                return Vec::new();
            }
            // u64 para alinhar os itens.
            let mut buf = vec![0u64; (size as usize).div_ceil(8)];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if PdhGetFormattedCounterArrayW(c, PDH_FMT_DOUBLE, &mut size, &mut count, Some(items)) != 0 {
                return Vec::new();
            }
            std::slice::from_raw_parts(items, count as usize)
                .iter()
                .filter(|i| i.FmtValue.CStatus == 0)
                .map(|i| (i.szName.to_string().unwrap_or_default().to_ascii_lowercase(), i.FmtValue.Anonymous.doubleValue))
                .collect()
        }
    }

    /// (% de uso, memória dedicada em uso) da placa `luid`. O uso segue o Gerenciador de
    /// Tarefas: soma os processos em cada motor e fica com o motor mais ocupado.
    fn read(&self, luid: &str) -> (Option<f64>, Option<u64>) {
        unsafe { PdhCollectQueryData(self.query) };
        let mut engines: Vec<(String, f64)> = Vec::new();
        for (name, v) in Self::values(self.engine) {
            // "pid_123_luid_…_phys_0_eng_0_engtype_3D": o motor é tudo a partir de "luid_".
            let Some(at) = name.find("luid_").filter(|_| name.contains(luid)) else { continue };
            let key = &name[at..];
            match engines.iter_mut().find(|e| e.0 == key) {
                Some(e) => e.1 += v,
                None => engines.push((key.to_string(), v)),
            }
        }
        let util = engines.iter().map(|e| e.1).reduce(f64::max).map(|u| u.clamp(0.0, 100.0));
        let mem: Vec<f64> = Self::values(self.memory).into_iter().filter(|(n, _)| n.contains(luid)).map(|e| e.1).collect();
        let used = (!mem.is_empty()).then(|| mem.iter().sum::<f64>() as u64);
        (util, used)
    }
}

impl Drop for Pdh {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.query) };
    }
}

/// DLL carregada na hora; solta no `Drop` de quem a usa.
fn load(names: &[PCWSTR]) -> Option<HMODULE> {
    names.iter().find_map(|&n| unsafe { LoadLibraryW(n) }.ok())
}

/// Função da DLL com a assinatura `F` (um `extern "system" fn`).
unsafe fn sym<F: Copy>(lib: HMODULE, name: &[u8]) -> Option<F> {
    let p = unsafe { GetProcAddress(lib, PCSTR(name.as_ptr())) }?;
    Some(unsafe { std::mem::transmute_copy(&p) })
}

/// AMD Display Library (vem com o driver Radeon). O log de energia (PMLog) dá a temperatura
/// da GPU e, nos APUs Ryzen, também a da CPU.
struct Adl {
    lib: HMODULE,
    ctx: *mut c_void,
    adapter: i32,
    pmlog: PmLog,
    destroy: unsafe extern "system" fn(*mut c_void) -> i32,
}

type PmLog = unsafe extern "system" fn(*mut c_void, i32, *mut PmLogData) -> i32;

/// ADLPMLogDataOutput: tamanho + 256 sensores (suportado, valor).
#[repr(C)]
struct PmLogData {
    size: i32,
    sensors: [[i32; 2]; 256],
}

const PMLOG_TEMPERATURE_EDGE: usize = 8;
const PMLOG_TEMPERATURE_HOTSPOT: usize = 27;
const PMLOG_TEMPERATURE_GFX: usize = 28;
const PMLOG_TEMPERATURE_CPU: usize = 32;
const PMLOG_CLK_CPUCLK: usize = 34;

/// Alocador que o ADL pede; o que ele aloca aqui não volta (só usamos chamadas com buffer nosso).
extern "system" fn adl_alloc(n: i32) -> *mut c_void {
    match std::alloc::Layout::from_size_align(n.max(1) as usize, 16) {
        Ok(l) => unsafe { std::alloc::alloc(l) as _ },
        Err(_) => std::ptr::null_mut(),
    }
}

impl Adl {
    fn open() -> Option<Self> {
        let lib = load(&[w!("atiadlxx.dll")])?;
        let adl = unsafe { Self::init(lib) };
        if adl.is_none() {
            unsafe {
                let _ = FreeLibrary(lib);
            }
        }
        adl
    }

    unsafe fn init(lib: HMODULE) -> Option<Self> {
        type Create = unsafe extern "system" fn(extern "system" fn(i32) -> *mut c_void, i32, *mut *mut c_void) -> i32;
        type Count = unsafe extern "system" fn(*mut c_void, *mut i32) -> i32;
        unsafe {
            let create: Create = sym(lib, b"ADL2_Main_Control_Create\0")?;
            let count: Count = sym(lib, b"ADL2_Adapter_NumberOfAdapters_Get\0")?;
            let pmlog: PmLog = sym(lib, b"ADL2_New_QueryPMLogData_Get\0")?;
            let destroy = sym(lib, b"ADL2_Main_Control_Destroy\0")?;
            let mut ctx = std::ptr::null_mut();
            if create(adl_alloc, 1, &mut ctx) != 0 {
                return None;
            }
            let mut n = 0;
            count(ctx, &mut n);
            // O primeiro adaptador que responde ao PMLog.
            let mut adl = Self { lib, ctx, adapter: -1, pmlog, destroy };
            adl.adapter = (0..n.clamp(0, 64)).find(|&i| adl.log(i).is_some())?;
            Some(adl)
        }
    }

    fn log(&self, adapter: i32) -> Option<Box<PmLogData>> {
        let mut d = Box::new(PmLogData { size: size_of::<PmLogData>() as i32, sensors: [[0; 2]; 256] });
        (unsafe { (self.pmlog)(self.ctx, adapter, &mut *d) } == 0).then_some(d)
    }

    /// (CPU, GPU) em °C e o clock da CPU em MHz.
    fn read(&self) -> (Option<f64>, Option<f64>, Option<f64>) {
        let Some(d) = self.log(self.adapter) else { return (None, None, None) };
        let get = |i: usize, max: i32| Some(d.sensors[i]).filter(|s| s[0] != 0 && s[1] > 0 && s[1] < max).map(|s| s[1] as f64);
        let t = |i| get(i, 150);
        let gpu = t(PMLOG_TEMPERATURE_EDGE).or_else(|| t(PMLOG_TEMPERATURE_GFX)).or_else(|| t(PMLOG_TEMPERATURE_HOTSPOT));
        (t(PMLOG_TEMPERATURE_CPU), gpu, get(PMLOG_CLK_CPUCLK, 10_000))
    }
}

impl Drop for Adl {
    fn drop(&mut self) {
        unsafe {
            (self.destroy)(self.ctx);
            let _ = FreeLibrary(self.lib);
        }
    }
}

/// NVIDIA Management Library (vem com o driver GeForce).
struct Nvml {
    lib: HMODULE,
    device: *mut c_void,
    temperature: unsafe extern "C" fn(*mut c_void, u32, *mut u32) -> i32,
    shutdown: unsafe extern "C" fn() -> i32,
}

impl Nvml {
    fn open() -> Option<Self> {
        let lib = load(&[w!("nvml.dll"), w!("C:\\Program Files\\NVIDIA Corporation\\NVSMI\\nvml.dll")])?;
        let nvml = unsafe { Self::init(lib) };
        if nvml.is_none() {
            unsafe {
                let _ = FreeLibrary(lib);
            }
        }
        nvml
    }

    unsafe fn init(lib: HMODULE) -> Option<Self> {
        unsafe {
            let init: unsafe extern "C" fn() -> i32 = sym(lib, b"nvmlInit_v2\0")?;
            let handle: unsafe extern "C" fn(u32, *mut *mut c_void) -> i32 = sym(lib, b"nvmlDeviceGetHandleByIndex_v2\0")?;
            let temperature = sym(lib, b"nvmlDeviceGetTemperature\0")?;
            let shutdown: unsafe extern "C" fn() -> i32 = sym(lib, b"nvmlShutdown\0")?;
            if init() != 0 {
                return None;
            }
            let mut device = std::ptr::null_mut();
            if handle(0, &mut device) != 0 {
                shutdown();
                return None;
            }
            Some(Self { lib, device, temperature, shutdown })
        }
    }

    fn read(&self) -> Option<f64> {
        let mut t = 0u32;
        // NVML_TEMPERATURE_GPU = 0.
        (unsafe { (self.temperature)(self.device, 0, &mut t) } == 0).then_some(t as f64)
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        unsafe {
            (self.shutdown)();
            let _ = FreeLibrary(self.lib);
        }
    }
}
