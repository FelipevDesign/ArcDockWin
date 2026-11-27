//! Fundo de vidro: o conteúdo atrás do dock desfocado pelo DWM (HostBackdropBrush),
//! recortado num retângulo de cantos arredondados (raio 0 na topbar).

use std::ffi::c_void;

use windows_numerics::Vector2;
use windows::System::{DispatcherQueue, DispatcherQueueController};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{CompositionRoundedRectangleGeometry, Compositor, ContainerVisual, SpriteVisual};
use std::cell::RefCell;
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
    compositor: Compositor,
    root: ContainerVisual,
    visual: SpriteVisual,
    shape: CompositionRoundedRectangleGeometry,
    /// Modo ilhas: um vidro por grupo (x, largura) e o último pedido, para não repetir.
    islands: RefCell<(Vec<(SpriteVisual, CompositionRoundedRectangleGeometry)>, Vec<(i32, i32)>)>,
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
            let root = compositor.CreateContainerVisual()?;
            root.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
            root.Children()?.InsertAtTop(&visual)?;
            target.SetRoot(&root)?;

            Ok(Self { _queue: queue, _target: target, compositor, root, visual, shape, islands: RefCell::new((Vec::new(), Vec::new())) })
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
        let _ = self.root.SetOpacity(opacity);
    }

    /// Ilhas: um retângulo de vidro por grupo, `(x, largura)` dentro da janela, altura `h`.
    /// Lista vazia volta ao vidro único.
    pub fn set_islands(&self, parts: &[(i32, i32)], h: f32, radius: f32) {
        let Ok(mut st) = self.islands.try_borrow_mut() else { return };
        if st.1 == parts {
            return;
        }
        st.1 = parts.to_vec();
        let Ok(children) = self.root.Children() else { return };
        for (v, _) in st.0.drain(..) {
            let _ = children.Remove(&v);
        }
        let _ = self.visual.SetIsVisible(parts.is_empty());
        for &(x, w) in parts {
            let make = || -> Result<(SpriteVisual, CompositionRoundedRectangleGeometry)> {
                let v = self.compositor.CreateSpriteVisual()?;
                v.SetBrush(&self.compositor.CreateHostBackdropBrush()?)?;
                v.SetOffset(windows_numerics::Vector3 { X: x as f32, Y: 0.0, Z: 0.0 })?;
                v.SetSize(Vector2 { X: w as f32, Y: h })?;
                let g = self.compositor.CreateRoundedRectangleGeometry()?;
                g.SetCornerRadius(Vector2 { X: radius, Y: radius })?;
                g.SetSize(Vector2 { X: w as f32, Y: h })?;
                v.SetClip(&self.compositor.CreateGeometricClipWithGeometry(&g)?)?;
                children.InsertAtTop(&v)?;
                Ok((v, g))
            };
            if let Ok(pair) = make() {
                st.0.push(pair);
            }
        }
    }
}
