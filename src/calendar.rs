//! Painel de calendário (clique na hora da topbar): data por extenso e o mês em grade.

use windows::Win32::System::SystemInformation::GetLocalTime;

use crate::panel::{Cell, CellStyle, Event, PanelContent, Widget, glyph};

const DAYS: [&str; 7] = ["domingo", "segunda-feira", "terça-feira", "quarta-feira", "quinta-feira", "sexta-feira", "sábado"];
const SHORT: [&str; 7] = ["dom", "seg", "ter", "qua", "qui", "sex", "sáb"];
const MONTHS: [&str; 12] = [
    "janeiro", "fevereiro", "março", "abril", "maio", "junho", "julho", "agosto", "setembro", "outubro",
    "novembro", "dezembro",
];
const PREV: u32 = 1;
const NEXT: u32 = 2;

/// Dia da semana (0 = domingo), algoritmo de Sakamoto.
fn weekday(y: i32, m: u32, d: u32) -> usize {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    (y + y / 4 - y / 100 + y / 400 + T[m as usize - 1] + d as i32).rem_euclid(7) as usize
}

fn days_in(y: i32, m: u32) -> u32 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Mês mostrado (o de hoje ao abrir).
pub struct Calendar {
    year: i32,
    month: u32,
}

impl Calendar {
    pub fn new() -> Self {
        let t = unsafe { GetLocalTime() };
        Self { year: t.wYear as i32, month: t.wMonth as u32 }
    }
}

impl PanelContent for Calendar {
    fn width(&self) -> f32 {
        300.0
    }

    fn build(&mut self) -> Vec<Widget> {
        let t = unsafe { GetLocalTime() };
        let (ty, tm, td) = (t.wYear as i32, t.wMonth as u32, t.wDay as u32);
        let title = format!(
            "{}, {} de {} de {}",
            DAYS[weekday(ty, tm, td)],
            td,
            MONTHS[tm as usize - 1],
            ty
        );

        let (y, m) = (self.year, self.month);
        let mut cells: Vec<Cell> =
            SHORT.iter().map(|d| Cell { text: (*d).into(), style: CellStyle::Header, id: None }).collect();
        // Sempre 6 semanas (a altura não muda ao trocar de mês), com os dias vizinhos apagados.
        let first = weekday(y, m, 1) as u32;
        let (py, pm) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
        let prev_days = days_in(py, pm);
        let days = days_in(y, m);
        for i in 0..42u32 {
            let (text, style) = if i < first {
                (prev_days - first + i + 1, CellStyle::Dim)
            } else if i - first < days {
                let d = i - first + 1;
                let today = y == ty && m == tm && d == td;
                (d, if today { CellStyle::Accent } else { CellStyle::Normal })
            } else {
                (i - first - days + 1, CellStyle::Dim)
            };
            cells.push(Cell { text: text.to_string(), style, id: None });
        }

        vec![
            Widget::Title(title),
            Widget::Heading {
                text: format!("{} de {}", MONTHS[m as usize - 1], y),
                buttons: vec![(glyph::CHEVRON_LEFT, PREV), (glyph::CHEVRON_RIGHT, NEXT)],
            },
            Widget::Grid { cols: 7, cells },
        ]
    }

    fn event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Click(PREV) => {
                (self.year, self.month) = if self.month == 1 { (self.year - 1, 12) } else { (self.year, self.month - 1) };
                true
            }
            Event::Click(NEXT) => {
                (self.year, self.month) = if self.month == 12 { (self.year + 1, 1) } else { (self.year, self.month + 1) };
                true
            }
            _ => false,
        }
    }
}
