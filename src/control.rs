//! Central de Controle (botão de controles da topbar), no estilo do macOS: cartões de Wi‑Fi,
//! Bluetooth, Modo Escuro e energia, mídia tocando agora e sliders de brilho e som. Os
//! painéis de cada item (wifi, bluetooth, volume, power) vivem aqui dentro: o círculo
//! liga/desliga por eles e, ao abrir um, o painel mostra o conteúdo dele com uma linha de
//! voltar no topo.

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_DWORD, RegSetKeyValueW};
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_APP, WM_SETTINGCHANGE,
};
use windows::core::w;

use crate::bluetooth::BluetoothPanel;
use crate::brightness;
use crate::media::{self, Action};
use crate::panel::{Card, Event, PanelContent, Tile, Widget, glyph};
use crate::power::PowerPanel;
use crate::status::{self, Net};
use crate::volume::VolumePanel;
use crate::wifi::WifiPanel;

/// Repassada pela topbar a cada STATS_MS (via `panel::notify`): relê bateria e mídia.
pub const TICK: u32 = WM_APP + 60;
/// Postada por media/brightness ao terminar uma leitura em segundo plano.
pub const WM_UPDATE: u32 = WM_APP + 61;
/// Glifo do botão na topbar.
pub const GLYPH: u16 = 0xE9E9;

/// Ids da central bem acima dos usados pelos painéis de dentro (o slider do volume passa direto).
const BASE: u32 = 50_000;
const BACK: u32 = BASE;
const WIFI: u32 = BASE + 1;
const WIFI_OPEN: u32 = BASE + 2;
const BT: u32 = BASE + 3;
const BT_OPEN: u32 = BASE + 4;
const POWER: u32 = BASE + 5;
const SOUND: u32 = BASE + 6;
const DARK: u32 = BASE + 7;
const BRIGHT: u32 = BASE + 8;
const PREV: u32 = BASE + 9;
const PLAY: u32 = BASE + 10;
const NEXT: u32 = BASE + 11;

const GLYPH_POWER: u16 = 0xE7E8;
const GLYPH_DARK: u16 = 0xE793;
const ETHERNET: u16 = 0xE839;
const NO_NET: u16 = 0xF384;
const VOLUME_HIGH: u16 = 0xE995;
const BRIGHT_LOW: u16 = 0xEC8A;
const BRIGHT_HIGH: u16 = 0xE706;

#[derive(Clone, Copy, PartialEq)]
enum View {
    Main,
    Wifi,
    Bluetooth,
    Sound,
    Power,
}

pub struct ControlCenter {
    view: View,
    wifi: WifiPanel,
    bt: BluetoothPanel,
    volume: VolumePanel,
    power: PowerPanel,
    /// Rede cabeada (só quando não há Wi‑Fi).
    wired: Net,
}

/// Glifos da fonte Segoe Fluent Icons.
fn battery_glyph(b: &status::Battery) -> u16 {
    let level = ((b.percent as u16 + 5) / 10).min(10);
    match (b.charging, level) {
        (true, l) => 0xEBAB + l,  // BatteryCharging0..10
        (false, 10) => 0xE83F,    // Battery10
        (false, l) => 0xE850 + l, // Battery0..9
    }
}

/// Liga/desliga o tema escuro do Windows e dos apps e avisa todas as janelas.
fn set_dark(dark: bool) {
    let v: u32 = (!dark).into();
    let key = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
    for name in [w!("AppsUseLightTheme"), w!("SystemUsesLightTheme")] {
        unsafe {
            let _ = RegSetKeyValueW(HKEY_CURRENT_USER, key, name, REG_DWORD.0, Some(&v as *const u32 as _), 4);
        }
    }
    // Em outra thread: um app travado não segura a interface.
    std::thread::spawn(|| unsafe {
        let _ = SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(w!("ImmersiveColorSet").as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            1000,
            None,
        );
    });
}

impl ControlCenter {
    pub fn new() -> Self {
        let wifi = WifiPanel::new();
        let wired = if wifi.summary().0 { Net::None } else { status::network(&status::Wlan::open()) };
        media::refresh(None);
        brightness::refresh();
        Self { view: View::Main, wifi, bt: BluetoothPanel::new(), volume: VolumePanel::new(), power: PowerPanel, wired }
    }

    fn sub(&mut self) -> &mut dyn PanelContent {
        match self.view {
            View::Wifi => &mut self.wifi,
            View::Bluetooth => &mut self.bt,
            View::Power => &mut self.power,
            View::Main | View::Sound => &mut self.volume,
        }
    }

    fn main(&mut self) -> Vec<Widget> {
        let (has_wifi, radio, ssid) = self.wifi.summary();
        let wifi = if has_wifi {
            let sub = match (radio, ssid) {
                (Some(false), _) => "Desligado".into(),
                (_, Some(name)) => name,
                _ => "Não conectado".into(),
            };
            // Sem como alternar o rádio: o círculo também abre a lista.
            let toggle = if radio.is_some() { WIFI } else { WIFI_OPEN };
            Tile { glyph: glyph::WIFI, title: "Wi‑Fi".into(), sub, on: radio != Some(false), toggle, open: WIFI_OPEN }
        } else {
            let (g, sub) = match self.wired {
                Net::None => (NO_NET, "Sem conexão"),
                _ => (ETHERNET, "Ethernet"),
            };
            Tile { glyph: g, title: "Rede".into(), sub: sub.into(), on: self.wired != Net::None, toggle: WIFI_OPEN, open: WIFI_OPEN }
        };
        // Ordem pensada para a grade: com mídia, ela ocupa a coluna da direita ao lado de
        // Wi‑Fi e Bluetooth.
        let mut cards = vec![Card::Tile(wifi)];
        if let Some(m) = media::current() {
            let cover = m.cover.and_then(|px| {
                tiny_skia::Pixmap::from_vec(px, tiny_skia::IntSize::from_wh(media::COVER, media::COVER)?)
            });
            let play = if m.playing { 0xE769 } else { 0xE768 };
            cards.push(Card::Media {
                cover,
                title: m.title,
                artist: m.artist,
                buttons: [(0xE892, PREV), (play, PLAY), (0xE893, NEXT)],
            });
        }
        let (bt_on, device) = self.bt.summary();
        if let Some(on) = bt_on {
            let sub = match device {
                Some(d) if on => d,
                _ => if on { "Ligado" } else { "Desligado" }.into(),
            };
            cards.push(Card::Tile(Tile { glyph: glyph::BLUETOOTH, title: "Bluetooth".into(), sub, on, toggle: BT, open: BT_OPEN }));
        }
        let dark = !crate::light_theme();
        let sub = if dark { "Ligado" } else { "Desligado" };
        cards.push(Card::Tile(Tile { glyph: GLYPH_DARK, title: "Modo Escuro".into(), sub: sub.into(), on: dark, toggle: DARK, open: DARK }));
        let (g, sub) = match status::battery() {
            Some(b) => (battery_glyph(&b), format!("Bateria {}%", b.percent)),
            None => (GLYPH_POWER, "Desligar, reiniciar…".into()),
        };
        cards.push(Card::Tile(Tile { glyph: g, title: "Energia".into(), sub, on: false, toggle: POWER, open: POWER }));

        if let Some(v) = brightness::level() {
            cards.push(Card::Slider { id: BRIGHT, title: "Tela".into(), value: v, left: (BRIGHT_LOW, None), right: BRIGHT_HIGH, button: None });
        }
        // Volume: o slider do painel de som, com os ids dele (o evento volta para ele).
        if let Some(Widget::Slider { id, button: Some((g, mute)), value, .. }) =
            self.volume.build().into_iter().find(|w| matches!(w, Widget::Slider { .. }))
        {
            cards.push(Card::Slider {
                id,
                title: "Som".into(),
                value,
                left: (g, Some(mute)),
                right: VOLUME_HIGH,
                button: Some((glyph::CHEVRON_RIGHT, SOUND)),
            });
        }
        vec![Widget::Cards(cards)]
    }
}

impl PanelContent for ControlCenter {
    fn width(&self) -> f32 {
        // Larga o bastante para "Modo Escuro" caber inteiro no bloco.
        360.0
    }

    fn padded(&self) -> bool {
        self.view != View::Main
    }

    fn liquid(&self) -> bool {
        true
    }

    fn build(&mut self) -> Vec<Widget> {
        if self.view == View::Main {
            return self.main();
        }
        let mut out = vec![Widget::Row {
            id: BACK,
            glyph: Some(glyph::CHEVRON_LEFT),
            text: "Central de Controle".into(),
            sub: None,
            selected: false,
        }];
        out.extend(self.sub().build());
        out
    }

    fn event(&mut self, ev: Event) -> bool {
        let view = match ev {
            Event::Click(BACK) => View::Main,
            Event::Click(WIFI_OPEN) => View::Wifi,
            Event::Click(BT_OPEN) => View::Bluetooth,
            Event::Click(SOUND) => View::Sound,
            Event::Click(POWER) => View::Power,
            Event::Click(WIFI) => {
                let on = self.wifi.summary().1 == Some(true);
                return self.wifi.set_radio(!on);
            }
            Event::Click(BT) => {
                let on = self.bt.summary().0 == Some(true);
                return self.bt.set_on(!on);
            }
            Event::Click(DARK) => {
                set_dark(crate::light_theme());
                return true;
            }
            Event::Slide(BRIGHT, v) => {
                brightness::set(v);
                return true;
            }
            Event::Click(id @ (PREV | PLAY | NEXT)) => {
                let a = match id {
                    PREV => Action::Previous,
                    PLAY => Action::PlayPause,
                    _ => Action::Next,
                };
                media::refresh(Some(a));
                return false;
            }
            // Resto: do painel aberto (no principal, só o slider do volume).
            ev => return self.sub().event(ev),
        };
        self.view = view;
        true
    }

    fn notify(&mut self, msg: u32, wparam: usize) -> bool {
        match msg {
            TICK => {
                media::refresh(None);
                return self.view == View::Main;
            }
            WM_UPDATE => return self.view == View::Main,
            _ => {}
        }
        // Todos ouvem: os blocos da tela principal mostram o estado deles.
        let a = self.wifi.notify(msg, wparam);
        let b = self.bt.notify(msg, wparam);
        let c = self.volume.notify(msg, wparam);
        a || b || c
    }
}
