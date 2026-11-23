//! The uptime section of the report PDF (ADR-0180): a table of the last
//! 24 hours, 7, 30 and 90 days, and under each row the daily bars of the
//! period's days, with a legend.

use chrono::NaiveDate;

use super::{fit, width, Doc, Rgb, BAD, BODY, GOOD, INK, MARGIN, MUTED, RULE, SHADE, WIDTH};
use crate::availability::report::Kind;
use crate::availability::uptime::{Bar, Row, Uptime};
use crate::availability::words;

/// Most rows drawn; the page lists all of them.
pub const MAX_ROWS: usize = 200;
const AMBER: Rgb = (0.85, 0.55, 0.05);
const NONE: Rgb = (0.86, 0.87, 0.89);
/// The name column's share of the width, the rest going to four windows.
const NAME: f32 = 0.52;
const ROW: f32 = 15.0;
const BARS: f32 = 9.0;

/// The color of a day: all up, 99 % or more, under 99 %, not measured.
pub(super) fn color(bar: &Bar) -> Rgb {
    match bar.availability {
        None => NONE,
        Some(_) if bar.down_seconds <= 0.0 => GOOD,
        Some(a) if a >= 99.0 => AMBER,
        Some(_) => BAD,
    }
}

fn window_color(value: Option<f64>) -> Rgb {
    match value {
        None => MUTED,
        Some(v) if v >= 100.0 => GOOD,
        Some(v) if v >= 99.0 => AMBER,
        Some(_) => BAD,
    }
}

fn swatch(doc: &mut Doc, x: f32, y: f32, color: Rgb, w: f32, h: f32) {
    doc.page.set_fill_rgb(color.0, color.1, color.2);
    doc.page.rect(x, y, w, h);
    doc.page.fill_nonzero();
}

fn columns(doc: &mut Doc, cells: [(&str, Rgb); 5], bold: bool) {
    let inner = WIDTH - 2.0 * MARGIN - 8.0;
    let name_room = inner * NAME;
    let each = (inner - name_room) / 4.0;
    let first = fit(cells[0].0, BODY, bold, name_room - 6.0);
    doc.text(MARGIN + 4.0, doc.y, BODY, bold, cells[0].1, &first);
    for (i, (text, color)) in cells[1..].iter().enumerate() {
        let right = MARGIN + 4.0 + name_room + each * (i as f32 + 1.0);
        let at = right - width(text, BODY, bold);
        doc.text(at, doc.y, BODY, bold, *color, text);
    }
}

fn head(doc: &mut Doc) {
    let lang = doc.language;
    doc.y -= ROW + 2.0;
    swatch(doc, MARGIN, doc.y - 4.0, SHADE, WIDTH - 2.0 * MARGIN, ROW);
    columns(
        doc,
        [
            (
                lang.pick("Service, server or check", "Service, serveur ou contrôle"),
                MUTED,
            ),
            (lang.pick("24 h", "24 h"), MUTED),
            (lang.pick("7 days", "7 jours"), MUTED),
            (lang.pick("30 days", "30 jours"), MUTED),
            (lang.pick("90 days", "90 jours"), MUTED),
        ],
        true,
    );
}

fn name(row: &Row) -> String {
    match (&row.check, row.kind) {
        (Some(check), _) => format!("    {} \u{b7} {check}", row.name),
        (None, Kind::Service) => row.name.clone(),
        (None, _) => match &row.site_name {
            Some(site) => format!("{} ({site})", row.name),
            None => row.name.clone(),
        },
    }
}

fn row(doc: &mut Doc, row: &Row, from: NaiveDate) {
    let lang = doc.language;
    let pct = |v: Option<f64>| (words::availability(v, lang), window_color(v));
    let (day, week, month, quarter) = (
        pct(row.windows.day),
        pct(row.windows.week),
        pct(row.windows.month),
        pct(row.windows.quarter),
    );
    doc.y -= ROW;
    let title = name(row);
    columns(
        doc,
        [
            (&title, INK),
            (&day.0, day.1),
            (&week.0, week.1),
            (&month.0, month.1),
            (&quarter.0, quarter.1),
        ],
        false,
    );
    // The period's days, at most the 90 there are.
    let days: Vec<&Bar> = row.days.iter().filter(|b| b.date >= from).collect();
    if !days.is_empty() {
        let inner = WIDTH - 2.0 * MARGIN - 8.0;
        let step = inner / days.len() as f32;
        let gap = (step * 0.2).min(1.0);
        let top = doc.y - 6.0;
        for (i, bar) in days.iter().enumerate() {
            let x = MARGIN + 4.0 + step * i as f32;
            swatch(doc, x, top - BARS, color(bar), step - gap, BARS);
        }
        doc.y = top - BARS;
    }
    doc.page.set_stroke_rgb(RULE.0, RULE.1, RULE.2);
    doc.page.set_line_width(0.5);
    doc.page.move_to(MARGIN, doc.y - 5.0);
    doc.page.line_to(WIDTH - MARGIN, doc.y - 5.0);
    doc.page.stroke();
    doc.y -= 4.0;
}

fn legend(doc: &mut Doc) {
    let lang = doc.language;
    doc.room(16.0);
    doc.y -= 14.0;
    let mut x = MARGIN;
    for (text, color) in [
        (lang.pick("Up all day", "Disponible toute la journée"), GOOD),
        (lang.pick("99 % or more", "99\u{a0}% ou plus"), AMBER),
        (lang.pick("Under 99 %", "Moins de 99\u{a0}%"), BAD),
        (lang.pick("Not measured", "Non mesuré"), NONE),
    ] {
        swatch(doc, x, doc.y - 1.0, color, 8.0, 8.0);
        doc.text(x + 11.0, doc.y, 8.0, false, MUTED, text);
        x += 11.0 + width(text, 8.0, false) + 16.0;
    }
    doc.y -= 4.0;
}

/// The uptime section, bars from `from`, the period's first local day.
pub(super) fn section(doc: &mut Doc, uptime: &Uptime, from: NaiveDate) {
    if uptime.rows.is_empty() {
        return;
    }
    let lang = doc.language;
    doc.heading(lang.pick("Uptime", "Disponibilité"));
    doc.paragraph(
        lang.pick(
            "Availability over the last 24 hours, 7, 30 and 90 days before the end of the period, and one bar per day of the period. Checks without an outage in 90 days are left out.",
            "Disponibilité sur les dernières 24 heures, 7, 30 et 90 jours avant la fin de la période, et une barre par jour de la période. Les contrôles sans panne en 90 jours n’apparaissent pas.",
        ),
        MUTED,
    );
    legend(doc);
    doc.room(ROW * 2.0 + BARS + 12.0);
    head(doc);
    for r in uptime.rows.iter().take(MAX_ROWS) {
        if doc.y - (ROW + BARS + 12.0) < super::BOTTOM {
            doc.new_page();
            head(doc);
        }
        row(doc, r, from);
    }
    let more = uptime.rows.len().saturating_sub(MAX_ROWS);
    if more > 0 {
        let text = match lang {
            words::Language::En => format!("And {more} more, on the Reports page."),
            words::Language::Fr => format!("Et {more} de plus, sur la page Rapports."),
        };
        doc.paragraph(&text, MUTED);
    }
    doc.y -= 6.0;
}
