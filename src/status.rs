//! Estado do sistema mostrado na topbar: relógio, bateria, volume e rede.

use std::ffi::c_void;

use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, PROPERTYKEY, WPARAM};
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::{
    AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE, EDataFlow, ERole, IMMDeviceEnumerator,
    IMMNotificationClient, IMMNotificationClient_Impl, MMDeviceEnumerator, eConsole, eRender,
};
use windows::Win32::NetworkManagement::IpHelper::{GetBestInterface, GetIfEntry2, MIB_IF_ROW2};
use windows::Win32::NetworkManagement::WiFi::{
    WLAN_CONNECTION_ATTRIBUTES, WLAN_INTERFACE_INFO_LIST, WlanCloseHandle, WlanEnumInterfaces,
    WlanFreeMemory, WlanOpenHandle, WlanQueryInterface, wlan_interface_state_connected,
    wlan_intf_opcode_current_connection,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{PCWSTR, Result, implement};

/// Mensagem postada na janela da topbar pelo áudio: wParam 0 = volume/mudo mudou,
/// 1 = dispositivo de saída padrão trocou, 2 = um dispositivo entrou, saiu ou mudou de estado.
pub const WM_AUDIO: u32 = WM_APP + 2;

/// Texto do relógio (ex.: "qua 19 nov  14:32") e quantos ms faltam para a próxima troca
/// (próximo minuto, ou próximo segundo com os segundos ligados). Formato pelas Configurações.
pub fn clock() -> (String, u32) {
    const DAYS: [&str; 7] = ["dom", "seg", "ter", "qua", "qui", "sex", "sáb"];
    const MONTHS: [&str; 12] =
        ["jan", "fev", "mar", "abr", "mai", "jun", "jul", "ago", "set", "out", "nov", "dez"];
    let t = unsafe { GetLocalTime() };
    let (h24, secs) = (crate::config::clock_24h(), crate::config::clock_seconds());
    let zh = crate::i18n::is_traditional_chinese();
    let sec = if secs { format!(":{:02}", t.wSecond) } else { String::new() };
    let time = if h24 {
        format!("{:02}:{:02}{sec}", t.wHour, t.wMinute)
    } else {
        let h = (t.wHour + 11) % 12 + 1;
        let pm = t.wHour >= 12;
        match zh {
            true => format!("{}{h}:{:02}{sec}", if pm { "下午 " } else { "上午 " }, t.wMinute),
            false => format!("{h}:{:02}{sec} {}", t.wMinute, if pm { "PM" } else { "AM" }),
        }
    };
    let date = if zh {
        format!("{}月{}日", t.wMonth, t.wDay)
    } else if crate::i18n::is_english() {
        const D: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        format!("{} {} {}", D[t.wDayOfWeek as usize % 7], M[(t.wMonth as usize + 11) % 12], t.wDay)
    } else {
        format!("{} {} {}", DAYS[t.wDayOfWeek as usize % 7], t.wDay, MONTHS[(t.wMonth as usize + 11) % 12])
    };
    let text = if crate::config::clock_date() { format!("{date}  {time}") } else { time };
    // Um pouco depois da virada, para não acordar ainda no minuto (segundo) anterior.
    let ms = (t.wMilliseconds as u32).min(999);
    let ms = if secs { 1000 - ms + 20 } else { (60 - t.wSecond as u32 % 60) * 1000 - ms + 20 };
    (text, ms)
}

#[derive(Clone, Copy, PartialEq)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

/// Bateria, se houver (BatteryFlag 128 = sem bateria; 255 = desconhecido).
pub fn battery() -> Option<Battery> {
    let mut s = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut s) }.ok()?;
    if s.BatteryFlag & 128 != 0 {
        return None;
    }
    Some(Battery { percent: s.BatteryLifePercent.min(100), charging: s.ACLineStatus == 1 })
}

#[derive(Clone, Copy, PartialEq)]
pub enum Net {
    None,
    Ethernet,
    /// Nível do sinal: 0..=3.
    Wifi(u8),
}

/// Sessão da WlanAPI, aberta uma vez e reaproveitada a cada consulta.
pub struct Wlan(HANDLE);

impl Wlan {
    pub fn open() -> Self {
        let (mut version, mut h) = (0u32, HANDLE::default());
        if unsafe { WlanOpenHandle(2, None, &mut version, &mut h) } != 0 {
            h = HANDLE::default();
        }
        Self(h)
    }

    /// Qualidade do sinal (0..100) da primeira interface Wi-Fi conectada.
    fn signal(&self) -> Option<u32> {
        if self.0.is_invalid() {
            return None;
        }
        unsafe {
            let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
            if WlanEnumInterfaces(self.0, None, &mut list) != 0 || list.is_null() {
                return None;
            }
            let n = (*list).dwNumberOfItems as usize;
            let infos = std::slice::from_raw_parts((*list).InterfaceInfo.as_ptr(), n);
            let mut out = None;
            for info in infos.iter().filter(|i| i.isState == wlan_interface_state_connected) {
                let (mut size, mut data) = (0u32, std::ptr::null_mut::<c_void>());
                let r = WlanQueryInterface(
                    self.0,
                    &info.InterfaceGuid,
                    wlan_intf_opcode_current_connection,
                    None,
                    &mut size,
                    &mut data,
                    None,
                );
                if r == 0 && !data.is_null() {
                    let attr = &*(data as *const WLAN_CONNECTION_ATTRIBUTES);
                    out = Some(attr.wlanAssociationAttributes.wlanSignalQuality);
                    WlanFreeMemory(data);
                    break;
                }
            }
            WlanFreeMemory(list as *const c_void);
            out
        }
    }
}

impl Drop for Wlan {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe { WlanCloseHandle(self.0, None) };
        }
    }
}

/// Conexão em uso: a interface da rota para a internet (nenhum pacote é enviado).
pub fn network(wlan: &Wlan) -> Net {
    let mut index = 0u32;
    // 8.8.8.8 (a ordem dos bytes não importa aqui).
    if unsafe { GetBestInterface(0x0808_0808, &mut index) } != 0 {
        return Net::None;
    }
    let mut row: MIB_IF_ROW2 = unsafe { std::mem::zeroed() };
    row.InterfaceIndex = index;
    // IF_TYPE_IEEE80211 = 71.
    if unsafe { GetIfEntry2(&mut row) }.is_ok() && row.Type == 71 {
        let q = wlan.signal().unwrap_or(0);
        return Net::Wifi((q / 25).min(3) as u8);
    }
    Net::Ethernet
}

fn post(hwnd: isize, what: usize) {
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut c_void)), WM_AUDIO, WPARAM(what), LPARAM(0));
    }
}

/// Volume ou mudo do dispositivo mudou (chamado numa thread do áudio).
#[implement(IAudioEndpointVolumeCallback)]
struct VolumeEvents(isize);

impl IAudioEndpointVolumeCallback_Impl for VolumeEvents_Impl {
    fn OnNotify(&self, _: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> Result<()> {
        post(self.0, 0);
        Ok(())
    }
}

/// Troca do dispositivo de saída padrão (chamado numa thread do áudio).
#[implement(IMMNotificationClient)]
struct DeviceEvents(isize);

impl IMMNotificationClient_Impl for DeviceEvents_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> Result<()> {
        post(self.0, 2);
        Ok(())
    }
    fn OnDeviceAdded(&self, _: &PCWSTR) -> Result<()> {
        post(self.0, 2);
        Ok(())
    }
    fn OnDeviceRemoved(&self, _: &PCWSTR) -> Result<()> {
        post(self.0, 2);
        Ok(())
    }
    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _: &PCWSTR) -> Result<()> {
        if flow == eRender && role == eConsole {
            post(self.0, 1);
        }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> Result<()> {
        Ok(())
    }
}

/// Volume da saída padrão via Core Audio; avisa a janela por WM_AUDIO em vez de consultar.
pub struct Audio {
    devices: IMMDeviceEnumerator,
    device_events: IMMNotificationClient,
    volume_events: IAudioEndpointVolumeCallback,
    endpoint: Option<IAudioEndpointVolume>,
}

impl Audio {
    pub fn new(hwnd: HWND) -> Option<Self> {
        unsafe {
            let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
            let device_events: IMMNotificationClient = DeviceEvents(hwnd.0 as isize).into();
            let _ = devices.RegisterEndpointNotificationCallback(&device_events);
            let mut audio = Self {
                devices,
                device_events,
                volume_events: VolumeEvents(hwnd.0 as isize).into(),
                endpoint: None,
            };
            audio.bind();
            Some(audio)
        }
    }

    /// Liga ao dispositivo de saída padrão atual (de novo a cada troca).
    pub fn bind(&mut self) {
        unsafe {
            if let Some(old) = self.endpoint.take() {
                let _ = old.UnregisterControlChangeNotify(&self.volume_events);
            }
            let endpoint = self
                .devices
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .and_then(|d| d.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None));
            if let Ok(e) = endpoint {
                let _ = e.RegisterControlChangeNotify(&self.volume_events);
                self.endpoint = Some(e);
            }
        }
    }

    /// Nível (0..1) e mudo.
    pub fn state(&self) -> Option<(f32, bool)> {
        let e = self.endpoint.as_ref()?;
        unsafe { Some((e.GetMasterVolumeLevelScalar().ok()?, e.GetMute().ok()?.as_bool())) }
    }

    pub fn set_level(&self, level: f32) {
        if let Some(e) = &self.endpoint {
            unsafe {
                let _ = e.SetMasterVolumeLevelScalar(level.clamp(0.0, 1.0), std::ptr::null());
            }
        }
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        unsafe {
            if let Some(e) = self.endpoint.take() {
                let _ = e.UnregisterControlChangeNotify(&self.volume_events);
            }
            let _ = self.devices.UnregisterEndpointNotificationCallback(&self.device_events);
        }
    }
}

/// Uso de CPU e RAM do sistema, em %. A CPU é a média desde a leitura anterior.
pub struct Usage {
    prev: Option<(u64, u64)>,
}

impl Usage {
    pub fn new() -> Self {
        let mut u = Self { prev: None };
        u.cpu();
        u
    }

    /// % de CPU desde a última chamada (None na primeira).
    pub fn cpu(&mut self) -> Option<u8> {
        use windows::Win32::Foundation::FILETIME;
        use windows::Win32::System::Threading::GetSystemTimes;
        let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
        unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
        let ft = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
        // O tempo de kernel já inclui o ocioso.
        let (busy_total, idle) = (ft(kernel) + ft(user), ft(idle));
        let prev = self.prev.replace((busy_total, idle))?;
        let total = busy_total.saturating_sub(prev.0);
        if total == 0 {
            return None;
        }
        let used = total.saturating_sub(idle.saturating_sub(prev.1));
        Some(((used * 100 + total / 2) / total).min(100) as u8)
    }

    /// % de memória física em uso.
    pub fn ram(&self) -> Option<u8> {
        use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        unsafe { GlobalMemoryStatusEx(&mut m) }.ok()?;
        Some(m.dwMemoryLoad.min(100) as u8)
    }
}
