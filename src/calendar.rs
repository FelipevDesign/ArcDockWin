//! Painel de calendário (clique na hora da topbar): data por extenso e o mês em grade.

use windows::Win32::System::SystemInformation::GetLocalTime;

use crate::panel::{Cell, CellStyle, Event, PanelContent, Widget, glyph};

const DAYS: [&str; 7] = ["domingo", "segunda-feira", "terça-feira", "quarta-feira", "quinta-feira", "sexta-feira", "sábado"];
const DAYS_EN: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const SHORT_EN: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS_EN: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December",
];
const SHORT: [&str; 7] = ["dom", "seg", "ter", "qua", "qui", "sex", "sáb"];
const MONTHS: [&str; 12] = [
    "janeiro", "fevereiro", "março", "abril", "maio", "junho", "julho", "agosto", "setembro", "outubro",
    "novembro", "dezembro",
];
const PREV: u32 = 1;
const NEXT: u32 = 2;
/// Relógios mundiais: escolher cidades, pronto, e cada cidade (CITY + índice).
const PICK: u32 = 3;
const DONE: u32 = 4;
const CITY: u32 = 100;

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
    /// Escolhendo as cidades dos relógios mundiais.
    picking: bool,
}

impl Calendar {
    pub fn new() -> Self {
        let t = unsafe { GetLocalTime() };
        Self { year: t.wYear as i32, month: t.wMonth as u32, picking: false }
    }
}

impl PanelContent for Calendar {
    fn width(&self) -> f32 {
        300.0
    }
    fn centered(&self) -> bool {
        true
    }

    fn build(&mut self) -> Vec<Widget> {
        if self.picking {
            let chosen = crate::topextra::chosen_cities();
            let mut out = vec![Widget::Heading { text: crate::i18n::tr("Relógios mundiais"), buttons: vec![(glyph::CHECK, DONE)] }];
            for (i, (name, _)) in crate::topextra::CITIES.iter().enumerate() {
                out.push(Widget::Row { id: CITY + i as u32, glyph: None, text: (*name).into(), sub: None, selected: chosen.contains(&i) });
            }
            return out;
        }
        let mut out = self.month_view();
        if crate::extras::on("bar_world") {
            out.push(Widget::Separator);
            out.push(Widget::Heading { text: crate::i18n::tr("Relógios mundiais"), buttons: vec![(0xE710, PICK)] });
            let rows = crate::topextra::world_rows(CITY);
            if rows.is_empty() {
                out.push(Widget::Text(crate::i18n::tr("Toque no + para escolher as cidades")));
            }
            out.extend(rows);
        }
        out
    }

    fn event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Click(PICK) => {
                self.picking = true;
                true
            }
            Event::Click(DONE) => {
                self.picking = false;
                true
            }
            Event::Click(id) if self.picking && id >= CITY => {
                crate::topextra::toggle_city((id - CITY) as usize);
                true
            }
            ev => self.month_event(ev),
        }
    }
}

impl Calendar {
    fn month_view(&mut self) -> Vec<Widget> {
        let t = unsafe { GetLocalTime() };
        let (ty, tm, td) = (t.wYear as i32, t.wMonth as u32, t.wDay as u32);
        let title = if crate::i18n::is_traditional_chinese() {
            const DAYS_ZH: [&str; 7] = ["星期日", "星期一", "星期二", "星期三", "星期四", "星期五", "星期六"];
            format!("{ty}年{tm}月{td}日 {}", DAYS_ZH[weekday(ty, tm, td)])
        } else if crate::i18n::is_english() {
            format!("{}, {} {}, {}", DAYS_EN[weekday(ty, tm, td)], MONTHS_EN[tm as usize - 1], td, ty)
        } else {
            format!(
                "{}, {} de {} de {}",
                DAYS[weekday(ty, tm, td)],
                td,
                MONTHS[tm as usize - 1],
                ty
            )
        };

        let (y, m) = (self.year, self.month);
        let mut cells: Vec<Cell> = if crate::i18n::is_traditional_chinese() {
            ["日", "一", "二", "三", "四", "五", "六"]
                .iter()
                .map(|d| Cell { text: (*d).into(), style: CellStyle::Header, id: None })
                .collect()
        } else if crate::i18n::is_english() {
            SHORT_EN.iter().map(|d| Cell { text: (*d).into(), style: CellStyle::Header, id: None }).collect()
        } else {
            SHORT.iter().map(|d| Cell { text: (*d).into(), style: CellStyle::Header, id: None }).collect()
        };
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

        let month_title = if crate::i18n::is_traditional_chinese() {
            format!("{y}年{m}月")
        } else if crate::i18n::is_english() {
            format!("{} {}", MONTHS_EN[m as usize - 1], y)
        } else {
            format!("{} de {}", MONTHS[m as usize - 1], y)
        };
        vec![
            Widget::Title(title),
            Widget::Heading {
                text: month_title,
                buttons: vec![(glyph::CHEVRON_LEFT, PREV), (glyph::CHEVRON_RIGHT, NEXT)],
            },
            Widget::Grid { cols: 7, cells },
        ]
    }

    fn month_event(&mut self, ev: Event) -> bool {
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
