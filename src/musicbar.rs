//! Quadro de música ao lado do dock (como uma ilha à parte): capa, título e artista, botões
//! anterior / tocar-pausar / próxima e a barra da faixa andando. Clique na barra pula para o
//! ponto; clique na capa ou no título abre o app que está tocando. Some quando nada toca há
//! um tempo, quando o dock se esconde, ou com a opção desligada.

use std::cell::RefCell;

use tiny_skia::{FillRule, Pixmap, PixmapMut, PixmapPaint, Stroke, Transform};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::DirectWrite::{DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

use crate::panel::{self, Buffer, Font, Text, TextOp};
use crate::topbar::rounded_rect;

const WIDTH: f32 = 340.0;
const TIMER_ID: usize = 1;

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Prev,
    Play,
    Next,
    Bar,
    Open,
}

struct State {
    hwnd: HWND,
    glass: HWND,
    fx: Option<crate::glass::Glass>,
    buf: Option<Buffer>,
    /// Onde o dock está (tela) e a escala dele; `None` = dock escondido.
    anchor: Option<(RECT, f32)>,
    pos: POINT,
    size: (i32, i32),
    visible: bool,
    zones: Vec<([f32; 4], Hit)>,
    /// Barra da faixa (x, largura) para o clique virar posição.
    bar: (f32, f32),
}

thread_local! {
    static ST: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn ensure() -> Option<HWND> {
    if let Some(h) = ST.with_borrow(|s| s.as_ref().map(|s| s.hwnd)) {
        return Some(h);
    }
    unsafe {
        let module = GetModuleHandleW(None).ok()?;
        let cursor = LoadCursorW(None, IDC_HAND).unwrap_or_default();
        let wc = WNDCLASSW { lpfnWndProc: Some(proc_), hInstance: module.into(), hCursor: cursor, lpszClassName: w!("ArcDockMusic"), ..Default::default() };
        RegisterClassW(&wc);
        let glass = CreateWindowExW(
            WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP,
            w!("ArcDockWinGlass"),
            w!("ArcDockWin Music Glass"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(module.into()),
            None,
        )
        .ok()?;
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            w!("ArcDockMusic"),
            w!("ArcDockWin Música"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            Some(glass),
            None,
            Some(module.into()),
            None,
        )
        .ok()?;
        let fx = crate::glass::Glass::new(glass, 18.0).ok();
        ST.with_borrow_mut(|s| {
            *s = Some(State {
                hwnd,
                glass,
                fx,
                buf: None,
                anchor: None,
                pos: POINT::default(),
                size: (0, 0),
                visible: false,
                zones: Vec::new(),
                bar: (0.0, 1.0),
            })
        });
        Some(hwnd)
    }
}

/// O dock principal se mexeu: `rect` na tela (ou `None`, escondido). `resting` = dock parado
/// (sem ícones ampliados): só aí vale a largura; nos outros quadros (aparecer/sumir) o quadro
/// acompanha só o movimento vertical, no mesmo passo do dock.
pub fn anchor(rect: Option<RECT>, scale: f32, resting: bool) {
    if !crate::extras::on("tool_music") {
        return;
    }
    enum Next {
        Nothing,
        Redraw,
        Move,
    }
    let next = ST.with_borrow_mut(|s| {
        let Some(s) = s.as_mut() else {
            if resting {
                PENDING.with(|p| p.set(rect.map(|r| (r, scale))));
            }
            return Next::Nothing;
        };
        let Some(r) = rect else {
            s.anchor = None;
            return Next::Redraw;
        };
        let old = s.anchor.map(|(a, _)| a);
        let mut a = old.unwrap_or(r);
        if resting || old.is_none() {
            a.left = r.left;
            a.right = r.right;
        }
        a.top = r.top;
        a.bottom = r.bottom;
        let same_size = old.is_some_and(|o| o.bottom - o.top == a.bottom - a.top && o.left == a.left && o.right == a.right);
        let moved = old.is_none_or(|o| o.top != a.top);
        s.anchor = Some((a, scale));
        if s.visible && same_size {
            if moved { Next::Move } else { Next::Nothing }
        } else {
            Next::Redraw
        }
    });
    match next {
        Next::Redraw => update(),
        Next::Move => move_now(),
        Next::Nothing => {}
    }
}

/// Onde o quadro fica: à direita do dock, com folga para a ampliação dos ícones; sem
/// espaço, à esquerda.
fn place(dock: RECT, w: i32, scale: f32) -> POINT {
    let h = (dock.bottom - dock.top).max(40);
    let grow = if crate::config::magnify() { ((crate::config::magnify_scale() - 1.0) * h as f32 * 0.6) as i32 } else { 0 };
    let gap = (12.0 * scale) as i32 + grow;
    let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe {
        let _ = GetMonitorInfoW(MonitorFromPoint(POINT { x: dock.left, y: dock.top }, MONITOR_DEFAULTTONEAREST), &mut mi);
    }
    let mut x = dock.right + gap;
    if x + w > mi.rcMonitor.right - 4 {
        x = dock.left - gap - w;
    }
    POINT { x, y: dock.top }
}

/// Só move (o desenho é o mesmo): acompanha o dock aparecendo/sumindo.
fn move_now() {
    ST.with_borrow_mut(|s| {
        let Some(s) = s.as_mut() else { return };
        let Some((dock, scale)) = s.anchor else { return };
        let pos = place(dock, s.size.0, scale);
        unsafe {
            let flags = SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER;
            let _ = SetWindowPos(s.glass, None, pos.x, pos.y, 0, 0, flags);
            let _ = SetWindowPos(s.hwnd, None, pos.x, pos.y, 0, 0, flags);
        }
        s.pos = pos;
    });
}

thread_local! {
    static PENDING: std::cell::Cell<Option<(RECT, f32)>> = const { std::cell::Cell::new(None) };
}

/// Opção ligada/desligada.
pub fn sync() {
    if !crate::extras::on("tool_music") {
        hide();
        return;
    }
    update();
}

/// Redesenha (a cada segundo pelo relógio das ferramentas, e ao mudar a música ou o dock).
pub fn update() {
    if !crate::extras::on("tool_music") {
        return hide();
    }
    let media = crate::media::current();
    let Some(m) = media else { return hide() };
    let Some(hwnd) = ensure() else { return };
    if let Some(p) = PENDING.with(|p| p.take()) {
        ST.with_borrow_mut(|s| {
            if let Some(s) = s.as_mut() {
                s.anchor = Some(p);
            }
        });
    }
    let Some((dock, scale)) = ST.with_borrow(|s| s.as_ref().and_then(|s| s.anchor)) else { return hide() };
    let h = (dock.bottom - dock.top).max(40);
    let w = (WIDTH * scale).round() as i32;
    let pos = place(dock, w, scale);
    draw(hwnd, &m, w, h, scale, pos);
    // Tocando: a barra anda; parado, nada a animar.
    unsafe {
        if m.playing {
            SetTimer(Some(hwnd), TIMER_ID, 250, None);
        } else {
            let _ = KillTimer(Some(hwnd), TIMER_ID);
        }
    }
}

fn hide() {
    ST.with_borrow_mut(|s| {
        let Some(s) = s.as_mut() else { return };
        if s.visible {
            unsafe {
                let _ = KillTimer(Some(s.hwnd), TIMER_ID);
                let _ = ShowWindow(s.hwnd, SW_HIDE);
                let _ = ShowWindow(s.glass, SW_HIDE);
            }
            s.visible = false;
        }
        // Memória do desenho volta ao sistema enquanto escondido.
        s.buf = None;
    });
}

fn draw(hwnd: HWND, m: &crate::media::Media, w: i32, h: i32, k: f32, pos: POINT) {
    let light = crate::tools::light_style();
    let Some(buf) = Buffer::new(w, h) else { return };
    let bytes = unsafe { std::slice::from_raw_parts_mut(buf.bits, (w * h * 4) as usize) };
    bytes.fill(0);
    let (fw, fh) = (w as f32, h as f32);
    let pad = (10.0 * k).round();
    let radius = 18.0 * k;
    let cover = fh - 2.0 * pad;
    let x0 = pad + cover + 12.0 * k;
    let bw = 30.0 * k;
    let bx = fw - pad - 3.0 * bw;
    let bar_y = fh - pad - 3.0 * k;
    let mut zones = Vec::new();
    {
        let Some(mut pm) = PixmapMut::from_bytes(bytes, w as u32, h as u32) else { return };
        let id = Transform::identity();
        let (tint, line) = if light { ([246, 246, 248, 150], [255, 255, 255, 200]) } else { ([24, 24, 28, 120], [255, 255, 255, 38]) };
        if let Some(p) = rounded_rect(0.5, 0.5, fw - 1.0, fh - 1.0, radius) {
            pm.fill_path(&p, &panel::paint(tint), FillRule::Winding, id, None);
            pm.stroke_path(&p, &panel::paint(line), &Stroke { width: 1.0, ..Default::default() }, id, None);
        }
        // Capa (já em BGRA, igual ao buffer).
        if let Some(c) = &m.cover
            && let Some(src) = tiny_skia::IntSize::from_wh(crate::media::COVER, crate::media::COVER).and_then(|sz| Pixmap::from_vec(c.clone(), sz))
            && let Some(mask_path) = rounded_rect(pad, pad, cover, cover, 9.0 * k)
        {
            let mut mask = tiny_skia::Mask::new(w as u32, h as u32).unwrap_or_else(|| tiny_skia::Mask::new(1, 1).expect("mask"));
            mask.fill_path(&mask_path, FillRule::Winding, true, id);
            let s = cover / crate::media::COVER as f32;
            let paint = PixmapPaint { quality: tiny_skia::FilterQuality::Bicubic, ..Default::default() };
            pm.draw_pixmap(0, 0, src.as_ref(), &paint, Transform::from_row(s, 0.0, 0.0, s, pad, pad), Some(&mask));
        } else if let Some(p) = rounded_rect(pad, pad, cover, cover, 9.0 * k) {
            pm.fill_path(&p, &panel::paint([128, 128, 140, 90]), FillRule::Winding, id, None);
        }
        // Barra da faixa.
        let (bl, br) = (x0, fw - pad);
        let frac = if m.duration > 0.0 { (m.position_now() / m.duration).clamp(0.0, 1.0) as f32 } else { 0.0 };
        let track = if light { [0, 0, 0, 40] } else { [255, 255, 255, 46] };
        let fill = if light { [20, 20, 24, 200] } else { [255, 255, 255, 215] };
        if let Some(p) = rounded_rect(bl, bar_y, br - bl, 3.0 * k, 1.5 * k) {
            pm.fill_path(&p, &panel::paint(track), FillRule::Winding, id, None);
        }
        if m.duration > 0.0
            && let Some(p) = rounded_rect(bl, bar_y, ((br - bl) * frac).max(3.0 * k), 3.0 * k, 1.5 * k)
        {
            pm.fill_path(&p, &panel::paint(fill), FillRule::Winding, id, None);
        }
        zones.push(([bl, bar_y - 8.0 * k, br - bl, 19.0 * k], Hit::Bar));
        zones.push(([pad, pad, cover, cover], Hit::Open));
        zones.push(([x0, pad, bx - x0, bar_y - pad - 4.0 * k], Hit::Open));
    }
    // Textos e botões (DirectWrite).
    if let Ok(text) = Text::new(k) {
        let ink = if light { [0.1, 0.1, 0.12, 0.95] } else { [1.0, 1.0, 1.0, 0.95] };
        let dim = if light { [0.1, 0.1, 0.12, 0.6] } else { [1.0, 1.0, 1.0, 0.6] };
        let text_h = bar_y - pad - 6.0 * k;
        let mid = pad + text_h / 2.0;
        let tw = bx - x0 - 6.0 * k;
        let mut ops = vec![
            TextOp {
                text: m.title.encode_utf16().collect(),
                font: Font::Bold,
                rect: [x0, mid - 19.0 * k, tw, 19.0 * k],
                align: DWRITE_TEXT_ALIGNMENT_LEADING,
                color: ink,
                wrap: false,
                clip: None,
            },
            TextOp {
                text: m.artist.encode_utf16().collect(),
                font: Font::Small,
                rect: [x0, mid + 1.0 * k, tw, 17.0 * k],
                align: DWRITE_TEXT_ALIGNMENT_LEADING,
                color: dim,
                wrap: false,
                clip: None,
            },
        ];
        for (i, (glyph, hit)) in [(0xE892u16, Hit::Prev), (if m.playing { 0xE769 } else { 0xE768 }, Hit::Play), (0xE893, Hit::Next)].into_iter().enumerate() {
            let r = [bx + i as f32 * bw, mid - bw / 2.0, bw, bw];
            ops.push(TextOp { text: vec![glyph], font: Font::Icon, rect: r, align: DWRITE_TEXT_ALIGNMENT_CENTER, color: ink, wrap: false, clip: None });
            zones.push((r, hit));
        }
        text.draw(buf.memdc, w, h, &ops);
    }
    ST.with_borrow_mut(|s| {
        let Some(s) = s.as_mut() else { return };
        s.zones = zones;
        s.bar = (x0, fw - pad - x0);
        let moved = !s.visible || s.pos.x != pos.x || s.pos.y != pos.y || s.size != (w, h);
        unsafe {
            if moved {
                if let Some(fx) = &s.fx {
                    fx.set_radius(radius);
                    fx.resize(fw, fh);
                    fx.refresh();
                }
                let _ = SetWindowPos(s.glass, Some(HWND_TOPMOST), pos.x, pos.y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            }
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
            let _ = UpdateLayeredWindow(
                hwnd,
                None,
                Some(&pos),
                Some(&SIZE { cx: w, cy: h }),
                Some(buf.memdc),
                Some(&POINT::default()),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            if moved {
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), pos.x, pos.y, 0, 0, SWP_NOACTIVATE | SWP_NOSIZE | SWP_SHOWWINDOW);
            }
        }
        s.pos = pos;
        s.size = (w, h);
        s.visible = true;
        s.buf = Some(buf);
    });
}

fn click(x: f32, y: f32) {
    let Some((hit, bar)) = ST.with_borrow(|s| {
        let s = s.as_ref()?;
        let hit = s.zones.iter().find(|(r, _)| x >= r[0] && x < r[0] + r[2] && y >= r[1] && y < r[1] + r[3])?.1;
        Some((hit, s.bar))
    }) else {
        return;
    };
    use crate::media::Action;
    let action = match hit {
        Hit::Prev => Action::Previous,
        Hit::Play => Action::PlayPause,
        Hit::Next => Action::Next,
        Hit::Bar => {
            let Some(m) = crate::media::current().filter(|m| m.duration > 0.0) else { return };
            Action::Seek(((x - bar.0) / bar.1).clamp(0.0, 1.0) as f64 * m.duration)
        }
        Hit::Open => {
            if let Some(m) = crate::media::current().filter(|m| !m.app_id.is_empty()) {
                // Procura numa thread à parte (a busca da aba pode levar um instante).
                std::thread::spawn(move || focus_player(&m.app_id, &m.title));
            }
            return;
        }
    };
    crate::media::refresh(Some(action));
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_LBUTTONUP => {
                let (x, y) = ((lp.0 & 0xffff) as i16 as f32, ((lp.0 >> 16) & 0xffff) as i16 as f32);
                click(x, y);
                LRESULT(0)
            }
            WM_TIMER if wp.0 == TIMER_ID => {
                update();
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// Leva à frente a janela que está tocando: no navegador, a aba com o título da mídia (ex.:
/// o vídeo do YouTube), em vez de abrir uma janela nova. Sem achar, abre o app.
fn focus_player(app_id: &str, title: &str) {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
    use windows::Win32::UI::Accessibility::*;
    let id = app_id.to_lowercase();
    let title_l = title.to_lowercase();
    // Janelas do app: o nome do .exe aparece no id da mídia ("Chrome", "Spotify...").
    let wins: Vec<(HWND, String)> = top_windows()
        .into_iter()
        .filter(|(_, exe, _)| !exe.is_empty() && id.contains(exe.as_str()))
        .map(|(h, _, t)| (h, t))
        .collect();
    let bring = |h: HWND| unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let _ = SetForegroundWindow(h);
    };
    if let Some((h, _)) = wins.iter().find(|(_, t)| !title_l.is_empty() && t.to_lowercase().contains(&title_l)) {
        return bring(*h);
    }
    // A aba não está na frente: procura entre as abas (acessibilidade do navegador).
    if !title_l.is_empty() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            if let Ok(uia) = CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER) {
                let cond = uia.CreatePropertyCondition(UIA_ControlTypePropertyId, &windows::Win32::System::Variant::VARIANT::from(UIA_TabItemControlTypeId.0));
                for (h, _) in &wins {
                    let (Ok(cond), Ok(root)) = (cond.as_ref(), uia.ElementFromHandle(*h)) else { continue };
                    let Ok(tabs) = root.FindAll(TreeScope_Descendants, cond) else { continue };
                    for i in 0..tabs.Length().unwrap_or(0) {
                        let Ok(tab) = tabs.GetElement(i) else { continue };
                        let name = tab.CurrentName().map(|b| b.to_string()).unwrap_or_default().to_lowercase();
                        if name.contains(&title_l)
                            && let Ok(sel) = tab.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
                        {
                            let _ = sel.Select();
                            bring(*h);
                            return;
                        }
                    }
                }
            }
        }
    }
    match wins.first() {
        Some((h, _)) => bring(*h),
        None => crate::apps::launch(&format!("shell:AppsFolder\\{app_id}"), ""),
    }
}

/// Janelas visíveis de primeiro nível: (janela, nome do .exe sem extensão, título).
fn top_windows() -> Vec<(HWND, String, String)> {
    unsafe extern "system" fn each(h: HWND, lp: LPARAM) -> windows::core::BOOL {
        unsafe {
            let out = &mut *(lp.0 as *mut Vec<(HWND, String, String)>);
            if IsWindowVisible(h).as_bool() && GetWindow(h, GW_OWNER).map(|o| o.is_invalid()).unwrap_or(true) {
                let mut t = [0u16; 512];
                let n = GetWindowTextW(h, &mut t) as usize;
                if n > 0 {
                    let mut pid = 0u32;
                    GetWindowThreadProcessId(h, Some(&mut pid));
                    out.push((h, exe_name(pid), String::from_utf16_lossy(&t[..n])));
                }
            }
            windows::core::BOOL(1)
        }
    }
    let mut v: Vec<(HWND, String, String)> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut v as *mut _ as isize));
    }
    v
}

fn exe_name(pid: u32) -> String {
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 512];
        let mut n = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut n).is_ok();
        let _ = windows::Win32::Foundation::CloseHandle(h);
        if !ok {
            return String::new();
        }
        let p = String::from_utf16_lossy(&buf[..n as usize]);
        std::path::Path::new(&p).file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default()
    }
}
