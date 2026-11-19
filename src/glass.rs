//! Fundo de vidro: o conteúdo atrás do dock desfocado pelo DWM (HostBackdropBrush),
//! recortado num retângulo de cantos arredondados (raio 0 na topbar).

use std::ffi::c_void;

use windows_numerics::Vector2;
use windows::System::{DispatcherQueue, DispatcherQueueController};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{Compositor, CompositionRoundedRectangleGeometry, SpriteVisual};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT, DispatcherQueueOptions,
};
use windows::core::{BOOL, Interface, Result};

const DWMWA_USE_HOSTBACKDROPBRUSH: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(17);

pub struct Glass {
    _queue: Option<DispatcherQueueController>,
    _target: DesktopWindowTarget,
    visual: SpriteVisual,
    shape: CompositionRoundedRectangleGeometry,
}

impl Glass {
    /// `hwnd` precisa ter sido criada com WS_EX_NOREDIRECTIONBITMAP.
    pub fn new(hwnd: HWND, radius: f32) -> Result<Self> {
        unsafe {
            let enable = BOOL(1);
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_HOSTBACKDROPBRUSH,
                &enable as *const _ as *const c_void,
                size_of::<BOOL>() as u32,
            )?;

            // Uma fila por thread: o segundo vidro (topbar) reaproveita a do primeiro.
            let queue = match DispatcherQueue::GetForCurrentThread() {
                Ok(_) => None,
                Err(_) => Some(CreateDispatcherQueueController(DispatcherQueueOptions {
                    dwSize: size_of::<DispatcherQueueOptions>() as u32,
                    threadType: DQTYPE_THREAD_CURRENT,
                    apartmentType: DQTAT_COM_STA,
                })?),
            };
            let compositor = Compositor::new()?;
            let target = compositor
                .cast::<ICompositorDesktopInterop>()?
                .CreateDesktopWindowTarget(hwnd, true)?;

            let visual = compositor.CreateSpriteVisual()?;
            visual.SetBrush(&compositor.CreateHostBackdropBrush()?)?;
            visual.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;

            let shape = compositor.CreateRoundedRectangleGeometry()?;
            shape.SetCornerRadius(Vector2 { X: radius, Y: radius })?;
            visual.SetClip(&compositor.CreateGeometricClipWithGeometry(&shape)?)?;
            target.SetRoot(&visual)?;

            Ok(Self { _queue: queue, _target: target, visual, shape })
        }
    }

    pub fn resize(&self, w: f32, h: f32) {
        let _ = self.shape.SetSize(Vector2 { X: w, Y: h });
    }

    /// Opacidade do vidro (0..1), usada na animação de abrir dos painéis.
    pub fn set_opacity(&self, opacity: f32) {
        let _ = self.visual.SetOpacity(opacity);
    }
}
