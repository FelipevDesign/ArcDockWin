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

thread_local! {
    /// Fila do WinRT da thread (a Composition precisa dela). Fica aqui, e não no vidro que a
    /// criou, porque vive até o fim: antes de o processo sair, `shutdown` a encerra.
    static QUEUE: std::cell::RefCell<Option<DispatcherQueueController>> = const { std::cell::RefCell::new(None) };
}

/// Encerra a fila da thread (ShutdownQueueAsync) e processa as mensagens até ela terminar.
/// Sem isso, a janela interna da fila só cai na limpeza das `thread_local` do ExitProcess,
/// e destruir janelas ali derruba o processo (0xC000000D no ntdll) em toda saída do dock.
pub fn shutdown() {
    use windows_future::AsyncStatus;
    use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, MSG, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW, QS_ALLINPUT, TranslateMessage};
    let Some(controller) = QUEUE.with_borrow_mut(Option::take) else { return };
    let Ok(action) = controller.ShutdownQueueAsync() else { return };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut msg = MSG::default();
    while action.Status().is_ok_and(|s| s == AsyncStatus::Started) && std::time::Instant::now() < deadline {
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            MsgWaitForMultipleObjects(None, false, 50, QS_ALLINPUT);
        }
    }
}

pub struct Glass {
    _target: DesktopWindowTarget,
    compositor: Compositor,
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
            if DispatcherQueue::GetForCurrentThread().is_err() {
                let controller = CreateDispatcherQueueController(DispatcherQueueOptions {
                    dwSize: size_of::<DispatcherQueueOptions>() as u32,
                    threadType: DQTYPE_THREAD_CURRENT,
                    apartmentType: DQTAT_COM_STA,
                })?;
                QUEUE.with_borrow_mut(|q| *q = Some(controller));
            }
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

            Ok(Self { _target: target, compositor, visual, shape })
        }
    }

    /// Arredondamento dos cantos (o modo barra de tarefas usa 0).
    pub fn set_radius(&self, radius: f32) {
        let _ = self.shape.SetCornerRadius(Vector2 { X: radius, Y: radius });
    }

    pub fn resize(&self, w: f32, h: f32) {
        let _ = self.shape.SetSize(Vector2 { X: w, Y: h });
    }

    /// O DWM guarda o que está atrás e só re-amostra quando a janela muda; quando um app
    /// fecha ou minimiza por trás, o vidro fica com a cor antiga. Um pincel novo força a releitura.
    pub fn refresh(&self) {
        if let Ok(brush) = self.compositor.CreateHostBackdropBrush() {
            let _ = self.visual.SetBrush(&brush);
        }
    }

    /// Opacidade do vidro (0..1), usada na animação de abrir dos painéis.
    pub fn set_opacity(&self, opacity: f32) {
        let _ = self.visual.SetOpacity(opacity);
    }
}
