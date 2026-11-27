//! Mídia tocando agora (Windows.Media.Control): título, artista, capa e os botões
//! anterior/tocar/próximo. As chamadas WinRT rodam numa thread à parte, que guarda o estado
//! aqui e avisa a topbar com `control::WM_UPDATE`.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapBounds, BitmapDecoder, BitmapInterpolationMode, BitmapPixelFormat, BitmapTransform,
    ColorManagementMode, ExifOrientationMode,
};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// Lado (px) da capa guardada.
pub const COVER: u32 = 96;

#[derive(Clone, Copy)]
pub enum Action {
    Previous,
    PlayPause,
    Next,
    /// Pular para este ponto da faixa (segundos).
    Seek(f64),
}

#[derive(Clone)]
pub struct Media {
    pub title: String,
    pub artist: String,
    pub playing: bool,
    /// Capa COVER×COVER em BGRA pré-multiplicado.
    pub cover: Option<Vec<u8>>,
    /// Posição (segundos) no instante `at`, e duração da faixa (0 = desconhecida).
    pub position: f64,
    pub duration: f64,
    pub at: std::time::Instant,
    /// App que está tocando (para abrir ao clicar).
    pub app_id: String,
}

impl Media {
    /// Posição agora, andando sozinha enquanto toca.
    pub fn position_now(&self) -> f64 {
        let p = if self.playing { self.position + self.at.elapsed().as_secs_f64() } else { self.position };
        if self.duration > 0.0 { p.min(self.duration) } else { p }
    }
}

static NOW: Mutex<Option<Media>> = Mutex::new(None);
static READING: AtomicBool = AtomicBool::new(false);

/// O que está tocando (última leitura).
pub fn current() -> Option<Media> {
    NOW.lock().ok()?.clone()
}

/// Relê em segundo plano (com `action`, executa antes); avisa a topbar ao terminar.
pub fn refresh(action: Option<Action>) {
    if action.is_none() && READING.swap(true, Ordering::AcqRel) {
        return;
    }
    let hwnd = crate::topbar::hwnd().0 as isize;
    std::thread::spawn(move || {
        unsafe {
            let init = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            if let Some(a) = action {
                let _ = run(a);
                // O app leva um instante para trocar de faixa/estado.
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
            let media = read().ok().flatten();
            if let Ok(mut now) = NOW.lock() {
                *now = media;
            }
            if init {
                CoUninitialize();
            }
            if action.is_none() {
                READING.store(false, Ordering::Release);
            }
            let _ = PostMessageW(Some(HWND(hwnd as _)), crate::control::WM_UPDATE, WPARAM(0), LPARAM(0));
        }
    });
}

fn run(action: Action) -> windows::core::Result<bool> {
    let s = Manager::RequestAsync()?.join()?.GetCurrentSession()?;
    match action {
        Action::Previous => s.TrySkipPreviousAsync()?.join(),
        Action::PlayPause => s.TryTogglePlayPauseAsync()?.join(),
        Action::Next => s.TrySkipNextAsync()?.join(),
        Action::Seek(secs) => s.TryChangePlaybackPositionAsync((secs * 10_000_000.0) as i64)?.join(),
    }
}

fn read() -> windows::core::Result<Option<Media>> {
    let Ok(s) = Manager::RequestAsync()?.join()?.GetCurrentSession() else { return Ok(None) };
    let props = s.TryGetMediaPropertiesAsync()?.join()?;
    let (title, artist) = (props.Title()?.to_string(), props.Artist()?.to_string());
    if title.is_empty() {
        return Ok(None);
    }
    let playing = s.GetPlaybackInfo()?.PlaybackStatus()? == Status::Playing;
    // Mesma faixa: aproveita a capa já decodificada.
    let old = current().filter(|m| m.title == title && m.artist == artist);
    let cover = match old {
        Some(m) => m.cover,
        None => props.Thumbnail().ok().and_then(|t| cover(&t).ok()),
    };
    // Linha do tempo: a posição vem com a hora em que foi medida; corrige até agora.
    let (mut position, mut duration) = (0.0, 0.0);
    if let Ok(tl) = s.GetTimelineProperties() {
        let secs = |t: windows::Foundation::TimeSpan| t.Duration as f64 / 10_000_000.0;
        let start = tl.StartTime().map(secs).unwrap_or(0.0);
        duration = (tl.EndTime().map(secs).unwrap_or(0.0) - start).max(0.0);
        position = (tl.Position().map(secs).unwrap_or(0.0) - start).max(0.0);
        if playing && let Ok(upd) = tl.LastUpdatedTime() {
            let now = unsafe { windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime() };
            let now = ((now.dwHighDateTime as i64) << 32) | now.dwLowDateTime as i64;
            let late = (now - upd.UniversalTime) as f64 / 10_000_000.0;
            if (0.0..86_400.0).contains(&late) {
                position += late;
            }
        }
    }
    let app_id = s.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default();
    Ok(Some(Media { title, artist, playing, cover, position, duration, at: std::time::Instant::now(), app_id }))
}

/// Capa recortada no quadrado do centro e reduzida para COVER×COVER.
fn cover(r: &windows::Storage::Streams::IRandomAccessStreamReference) -> windows::core::Result<Vec<u8>> {
    let stream = r.OpenReadAsync()?.join()?;
    let dec = BitmapDecoder::CreateAsync(&stream)?.join()?;
    let (w, h) = (dec.PixelWidth()?.max(1), dec.PixelHeight()?.max(1));
    let k = COVER as f32 / w.min(h) as f32;
    let (sw, sh) = (((w as f32 * k).round() as u32).max(COVER), ((h as f32 * k).round() as u32).max(COVER));
    let t = BitmapTransform::new()?;
    t.SetScaledWidth(sw)?;
    t.SetScaledHeight(sh)?;
    t.SetInterpolationMode(BitmapInterpolationMode::Fant)?;
    t.SetBounds(BitmapBounds { X: (sw - COVER) / 2, Y: (sh - COVER) / 2, Width: COVER, Height: COVER })?;
    let px = dec
        .GetPixelDataTransformedAsync(
            BitmapPixelFormat::Bgra8,
            BitmapAlphaMode::Premultiplied,
            &t,
            ExifOrientationMode::RespectExifOrientation,
            ColorManagementMode::DoNotColorManage,
        )?
        .join()?
        .DetachPixelData()?;
    Ok(px.to_vec())
}
