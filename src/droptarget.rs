//! Arrastar arquivos para o dock (OLE): enquanto se arrasta, os ícones crescem sob o cursor
//! como com o mouse, e a prateleira aparece como lugar próprio no fim do dock. Soltar:
//! - na prateleira: guarda lá;
//! - num app: abre os arquivos com ele (opção "Soltar arquivos no dock");
//! - no resto do dock: programas e atalhos são fixados; o resto vai para a prateleira.

use std::cell::Cell;
use std::path::PathBuf;

use windows::Win32::Foundation::{HWND, LPARAM, POINT, POINTL, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL};
use windows::Win32::System::Ole::{
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl, OleInitialize, RegisterDragDrop,
    ReleaseStgMedium, RevokeDragDrop,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_MOUSEMOVE};
use windows_core::{Ref, implement};

thread_local! {
    /// Arrasto de arquivos em cima de algum dock agora.
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static FILES: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

pub fn dragging() -> bool {
    ACTIVE.get()
}

#[implement(IDropTarget)]
struct Target {
    hwnd: HWND,
}

fn files_of(data: &IDataObject) -> Vec<PathBuf> {
    let fmt = FORMATETC { cfFormat: 15, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
    let mut out = Vec::new();
    unsafe {
        let Ok(mut med) = data.GetData(&fmt) else { return out };
        let drop = HDROP(med.u.hGlobal.0);
        let n = DragQueryFileW(drop, u32::MAX, None);
        for i in 0..n {
            let mut buf = [0u16; 1024];
            let len = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
            out.push(String::from_utf16_lossy(&buf[..len]).into());
        }
        ReleaseStgMedium(&mut med);
    }
    out
}

impl Target {
    fn client(&self, pt: &POINTL) -> POINT {
        let mut p = POINT { x: pt.x, y: pt.y };
        unsafe {
            let _ = ScreenToClient(self.hwnd, &mut p);
        }
        p
    }

    /// Faz o dock reagir como ao mouse (ampliação sob o cursor).
    fn hover(&self, p: POINT) {
        let lp = LPARAM(((p.x as u16 as isize) | ((p.y as u16 as isize) << 16)) as isize);
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_MOUSEMOVE, WPARAM(0), lp);
        }
    }

    fn leave(&self) {
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), 0x02A3 /* WM_MOUSELEAVE */, WPARAM(0), LPARAM(0));
        }
    }
}

impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(&self, data: Ref<IDataObject>, _: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows_core::Result<()> {
        let files = data.ok().map(files_of).unwrap_or_default();
        let ok = !files.is_empty();
        FILES.with_borrow_mut(|f| *f = files);
        unsafe {
            *effect = if ok { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
        }
        if ok {
            ACTIVE.set(true);
            // A prateleira aparece como destino enquanto se arrasta.
            crate::apply_settings(false);
            self.hover(self.client(pt));
        }
        Ok(())
    }

    fn DragOver(&self, _: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows_core::Result<()> {
        let ok = ACTIVE.get();
        unsafe {
            *effect = if ok { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
        }
        if ok {
            self.hover(self.client(pt));
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows_core::Result<()> {
        if ACTIVE.replace(false) {
            crate::apply_settings(false);
        }
        self.leave();
        Ok(())
    }

    fn Drop(&self, data: Ref<IDataObject>, _: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows_core::Result<()> {
        let mut files = FILES.with_borrow_mut(std::mem::take);
        if files.is_empty() {
            files = data.ok().map(files_of).unwrap_or_default();
        }
        unsafe {
            *effect = if files.is_empty() { DROPEFFECT_NONE } else { DROPEFFECT_COPY };
        }
        let p = self.client(pt);
        // Decide com a prateleira ainda à mostra (o lugar dela é o alvo).
        crate::dropped_files(self.hwnd, files, p);
        ACTIVE.set(false);
        crate::apply_settings(false);
        self.leave();
        Ok(())
    }
}

/// Liga/desliga o arrastar de arquivos para a janela do dock.
pub fn set(hwnd: HWND, on: bool) {
    unsafe {
        let _ = OleInitialize(None);
        let _ = RevokeDragDrop(hwnd);
        if on {
            let t: IDropTarget = Target { hwnd }.into();
            let _ = RegisterDragDrop(hwnd, &t);
        }
    }
}
