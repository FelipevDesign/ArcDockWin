//! Painel de volume (clique no ícone de volume da topbar): nível, mudo e saída padrão.

use std::ffi::c_void;

use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, ERole, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, eCommunications, eConsole,
    eMultimedia, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ};
use windows::core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface, PCWSTR};

use crate::panel::{Event, PanelContent, Widget, glyph};
use crate::status;

/// PKEY_Device_FriendlyName (Functiondiscoverykeys_devpkey.h).
const FRIENDLY_NAME: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0), pid: 14 };

/// CPolicyConfigClient e IPolicyConfig (não documentados; os que o Windows usa para trocar
/// a saída padrão).
const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
const IID_POLICY_CONFIG: GUID = GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);

/// Só o que se usa da vtable de IPolicyConfig: SetDefaultEndpoint é o 11º método depois de
/// IUnknown (GetMixFormat ... SetPropertyValue vêm antes).
#[repr(C)]
struct PolicyConfigVtbl {
    base: IUnknown_Vtbl,
    _before: [usize; 10],
    set_default_endpoint: unsafe extern "system" fn(*mut c_void, PCWSTR, ERole) -> HRESULT,
}

/// Torna `id` a saída padrão para todos os papéis (console, multimídia e comunicações).
fn set_default(id: &[u16]) {
    unsafe {
        let Ok(unk) = CoCreateInstance::<_, IUnknown>(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL) else { return };
        let mut raw: *mut c_void = std::ptr::null_mut();
        if unk.query(&IID_POLICY_CONFIG, &mut raw).is_err() || raw.is_null() {
            return;
        }
        // Solta a referência do query ao sair.
        let _policy = IUnknown::from_raw(raw);
        let vtbl = &**(raw as *const *const PolicyConfigVtbl);
        for role in [eConsole, eMultimedia, eCommunications] {
            let _ = (vtbl.set_default_endpoint)(raw, PCWSTR(id.as_ptr()), role);
        }
    }
}

/// Id do dispositivo (terminado em zero).
fn device_id(d: &IMMDevice) -> Option<Vec<u16>> {
    unsafe {
        let p = d.GetId().ok()?;
        let mut id = p.as_wide().to_vec();
        CoTaskMemFree(Some(p.0 as *const c_void));
        id.push(0);
        Some(id)
    }
}

const SLIDER: u32 = 1;
const MUTE: u32 = 2;
/// Linhas de dispositivo: DEVICE + índice.
const DEVICE: u32 = 100;

pub struct VolumePanel {
    devices: Option<IMMDeviceEnumerator>,
    endpoint: Option<IAudioEndpointVolume>,
    /// Saídas ativas: id e nome amigável.
    list: Vec<(Vec<u16>, String)>,
    default: Vec<u16>,
}

impl VolumePanel {
    pub fn new() -> Self {
        let devices = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok() };
        let mut p = Self { devices, endpoint: None, list: Vec::new(), default: Vec::new() };
        p.load_list();
        p.bind();
        p
    }

    /// Saída padrão atual e o controle de volume dela.
    fn bind(&mut self) {
        self.endpoint = None;
        self.default.clear();
        let Some(devices) = &self.devices else { return };
        unsafe {
            if let Ok(d) = devices.GetDefaultAudioEndpoint(eRender, eConsole) {
                self.default = device_id(&d).unwrap_or_default();
                self.endpoint = d.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).ok();
            }
        }
    }

    fn load_list(&mut self) {
        self.list.clear();
        let Some(devices) = &self.devices else { return };
        unsafe {
            let Ok(coll) = devices.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) else { return };
            for i in 0..coll.GetCount().unwrap_or(0) {
                let Ok(d) = coll.Item(i) else { continue };
                let Some(id) = device_id(&d) else { continue };
                let name = d
                    .OpenPropertyStore(STGM_READ)
                    .and_then(|s| s.GetValue(&FRIENDLY_NAME))
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                let name = if name.is_empty() { "Dispositivo de áudio".to_string() } else { name };
                self.list.push((id, name));
            }
        }
    }

    fn state(&self) -> Option<(f32, bool)> {
        let e = self.endpoint.as_ref()?;
        unsafe { Some((e.GetMasterVolumeLevelScalar().ok()?, e.GetMute().ok()?.as_bool())) }
    }
}

impl PanelContent for VolumePanel {
    fn build(&mut self) -> Vec<Widget> {
        let mut out = vec![Widget::Title("Som".into())];
        if let Some((level, muted)) = self.state() {
            out.push(Widget::Slider {
                id: SLIDER,
                button: Some((crate::topbar::volume_glyph((level, muted)), MUTE)),
                value: level,
                label: format!("{}%", (level * 100.0).round() as u32),
            });
        }
        out.push(Widget::Separator);
        out.push(Widget::Heading { text: "Saída".into(), buttons: Vec::new() });
        if self.list.is_empty() {
            out.push(Widget::Text("Nenhum dispositivo de saída.".into()));
        }
        for (i, (id, name)) in self.list.iter().enumerate() {
            out.push(Widget::Row {
                id: DEVICE + i as u32,
                glyph: Some(glyph::SPEAKER),
                text: name.clone(),
                sub: None,
                selected: *id == self.default,
            });
        }
        out
    }

    fn event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Slide(SLIDER, v) => {
                if let Some(e) = &self.endpoint {
                    unsafe {
                        let _ = e.SetMasterVolumeLevelScalar(v, std::ptr::null());
                    }
                }
                true
            }
            Event::Click(MUTE) => {
                if let (Some(e), Some((_, muted))) = (&self.endpoint, self.state()) {
                    unsafe {
                        let _ = e.SetMute(!muted, std::ptr::null());
                    }
                }
                true
            }
            Event::Click(id) if id >= DEVICE => {
                let Some((dev, _)) = self.list.get((id - DEVICE) as usize) else { return false };
                if *dev != self.default {
                    set_default(dev);
                    self.bind();
                }
                true
            }
            _ => false,
        }
    }

    /// `status::WM_AUDIO`: 0 = volume/mudo mudou, 1 = saída padrão trocou, 2 = dispositivo
    /// entrou ou saiu.
    fn notify(&mut self, msg: u32, wparam: usize) -> bool {
        if msg != status::WM_AUDIO {
            return false;
        }
        match wparam {
            1 => self.bind(),
            2 => {
                self.load_list();
                self.bind();
            }
            _ => {}
        }
        true
    }
}
