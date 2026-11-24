//! Arrastar ícones do dock para reordenar, fixar (soltar um app aberto entre os fixados)
//! ou desafixar (soltar um fixado bem acima do dock), com os outros abrindo espaço animados.
//! O botão Iniciar (sempre o primeiro) não se move e nada é solto antes dele; Downloads e
//! Lixeira ficam fora da lista de apps, sempre no fim.

use std::time::Instant;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};

use crate::apps::{self, App};
use crate::render::{self, Slot};

/// Constante de tempo (s) com que os ícones deslizam até o lugar novo.
const SETTLE: f32 = 0.05;

#[derive(Default)]
pub struct Drag {
    press: Option<Press>,
    /// Deslocamento visual de cada app (key) em relação ao lugar no layout; cai até zero.
    offsets: Vec<(String, f32)>,
    last: Option<Instant>,
}

struct Press {
    key: String,
    x: f32,
    y: f32,
    /// Distância do clique ao centro do ícone.
    grab: f32,
    /// Já virou arrasto (o mouse andou alguns px).
    active: bool,
    /// Índice inicial: para onde volta um app aberto que não foi fixado.
    home: usize,
    was_pinned: bool,
    /// O app fora do dock (arrastado para cima para desafixar).
    held: Option<App>,
}

enum Spot {
    Out,
    At(usize, bool),
}

impl Drag {
    /// Botão pressionado sobre um app (o dock não atualiza a lista nem esconde enquanto isso).
    pub fn busy(&self) -> bool {
        self.press.is_some()
    }
}

/// Primeiro índice arrastável: o que vem depois do botão Iniciar.
fn first(apps: &[App]) -> usize {
    apps.iter().position(|a| a.source == apps::START).map_or(0, |i| i + 1)
}

impl crate::Dock {
    /// Slots como são desenhados: com o deslize dos ícones e o arrastado seguindo o mouse.
    pub(crate) fn shown_slots(&self) -> Vec<Slot> {
        let mut slots = self.slots();
        let press = self.drag.press.as_ref().filter(|p| p.active);
        for (s, a) in slots.iter_mut().zip(&self.apps) {
            match press {
                Some(p) if p.key == a.key => s.dx = self.mx - p.grab - s.size / 2.0 - s.x,
                _ => {
                    if let Some(o) = self.drag.offsets.iter().find(|o| o.0 == a.key) {
                        s.x += o.1;
                    }
                }
            }
        }
        slots
    }

    /// Muda a lista e anima os ícones da posição em que estavam até a nova.
    fn reflow(&mut self, f: impl FnOnce(&mut Self)) {
        let before: Vec<(String, f32)> = self
            .apps
            .iter()
            .zip(self.shown_slots())
            .map(|(a, s)| (a.key.clone(), s.x + s.dx))
            .collect();
        f(self);
        self.drag.offsets = self
            .apps
            .iter()
            .zip(self.slots())
            .filter_map(|(a, s)| {
                let (_, x) = before.iter().find(|b| b.0 == a.key)?;
                Some((a.key.clone(), x - s.x)).filter(|o| o.1.abs() > 0.3)
            })
            .collect();
        self.drag.last = Some(Instant::now());
        self.start_anim();
    }

    /// Um quadro do deslize; true enquanto algum ícone ainda está chegando.
    pub(crate) fn drag_step(&mut self) -> bool {
        let now = Instant::now();
        let dt = self.drag.last.map_or(0.0, |t| (now - t).as_secs_f32());
        self.drag.last = Some(now);
        let k = (-dt / SETTLE).exp();
        self.drag.offsets.retain_mut(|o| {
            o.1 *= k;
            o.1.abs() > 0.3
        });
        !self.drag.offsets.is_empty()
    }

    pub(crate) fn drag_down(&mut self, x: f32, y: f32) {
        let Some(i) = self.hit(x).filter(|&i| i >= first(&self.apps) && i < self.apps.len()) else { return };
        let Some(s) = self.slots().into_iter().nth(i) else { return };
        self.drag.press = Some(Press {
            key: self.apps[i].key.clone(),
            x,
            y,
            grab: x - (s.x + s.size / 2.0),
            active: false,
            home: i,
            was_pinned: self.apps[i].pinned,
            held: None,
        });
        unsafe { SetCapture(self.hwnd) };
    }

    pub(crate) fn drag_move(&mut self, x: f32, y: f32) {
        let gap = self.m.gap;
        let Some(p) = &mut self.drag.press else { return };
        if !p.active {
            if (x - p.x).abs().max((y - p.y).abs()) < gap / 2.0 {
                return;
            }
            p.active = true;
        }
        let key = p.key.clone();
        match self.spot(x, y) {
            Spot::Out => {
                let Some(i) = self.apps.iter().position(|a| a.key == key) else { return };
                self.reflow(|d| {
                    let app = d.apps.remove(i);
                    if let Some(p) = &mut d.drag.press {
                        p.held = Some(app);
                    }
                });
            }
            Spot::At(k, pin) => {
                let i = self.apps.iter().position(|a| a.key == key);
                if i.is_some_and(|i| i == k && self.apps[i].pinned == pin) {
                    return;
                }
                self.reflow(|d| {
                    let Some(p) = &mut d.drag.press else { return };
                    let mut app = match i {
                        Some(i) => d.apps.remove(i),
                        None => match p.held.take() {
                            Some(a) => a,
                            None => return,
                        },
                    };
                    app.pinned = pin;
                    d.apps.insert(k.min(d.apps.len()), app);
                });
            }
        }
    }

    /// Onde o app arrastado deve ficar com o mouse em (x, y).
    fn spot(&self, x: f32, y: f32) -> Spot {
        let Some(p) = &self.drag.press else { return Spot::Out };
        let slots = self.slots();
        let top = render::box_rect(&slots, &self.m, self.pixmap.height() as f32).map_or(0.0, |r| r.1 as f32);
        if p.was_pinned && y < top - self.m.base * 1.5 {
            return Spot::Out;
        }
        let cx = x - p.grab;
        let n = first(&self.apps);
        let mut k = n;
        let mut last_right = None;
        for (a, s) in self.apps.iter().zip(&slots).skip(n) {
            if a.pinned && a.key != p.key {
                k += (s.x + s.size / 2.0 < cx) as usize;
                last_right = Some(s.x + s.size);
            }
        }
        if p.was_pinned {
            return Spot::At(k, true);
        }
        // App aberto: fixa se for levado até perto dos fixados; senão volta ao lugar.
        let bound = match last_right {
            Some(r) => r + (self.m.gap + self.m.base) / 2.0,
            None => slots.get(n).map_or(0.0, |s| s.x),
        };
        if cx < bound { Spot::At(k, true) } else { Spot::At(p.home, false) }
    }

    /// Botão solto (ou captura perdida). Retorna true se foi um arrasto (não um clique).
    pub(crate) fn drag_up(&mut self) -> bool {
        let Some(active) = self.drag.press.as_ref().map(|p| p.active) else { return false };
        if active {
            self.reflow(|d| {
                let Some(p) = d.drag.press.take() else { return };
                let gone = p.held.as_ref().map(|a| a.source.clone());
                // Desafixado: se estiver aberto, continua na seção dos abertos.
                if let Some(mut app) = p.held {
                    if !app.windows.is_empty() {
                        app.pinned = false;
                        let at = first(&d.apps) + d.apps.iter().filter(|a| a.pinned && a.source != apps::START).count();
                        d.apps.insert(at, app);
                    }
                }
                apps::save_pinned(&d.apps, gone.as_deref());
            });
        }
        self.drag.press = None;
        unsafe {
            let _ = ReleaseCapture();
            // Com o mouse capturado a saída pode ter sido ignorada: rearma (avisa na hora se já saiu).
            let mut tme = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            self.tracking = TrackMouseEvent(&mut tme).is_ok();
        }
        active
    }
}
