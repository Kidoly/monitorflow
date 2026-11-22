//! A report as a PDF to send to a customer as it is (ADR-0155): a title
//! with the period, a short summary in plain words, how availability is
//! measured, then the details in tables, with page numbers.
//!
//! Written with `pdf-writer` and the standard Helvetica fonts every reader
//! has, in WinAnsi encoding, so no font is embedded: Latin text only, which
//! English and French need. Widths come from [`super::pdf_metrics`].

use chrono::{DateTime, Datelike, NaiveTime, TimeDelta, Timelike, Utc};
use chrono_tz::Tz;
use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref, Str, TextStr};

use super::pdf_metrics::{BOLD, HIGH, REGULAR};
use super::report::{MachineView, Report};
use super::uptime::Uptime;
use super::words::{self, Language};

const WIDTH: f32 = 595.0;
const HEIGHT: f32 = 842.0;
const MARGIN: f32 = 50.0;
/// Where a page's content starts, under its header.
const TOP: f32 = HEIGHT - MARGIN - 14.0;
const BOTTOM: f32 = 64.0;
const BODY: f32 = 9.5;
/// Most outages listed; the CSV has all of them.
pub const MAX_OUTAGES: usize = 300;

type Rgb = (f32, f32, f32);
const INK: Rgb = (0.12, 0.13, 0.15);
const MUTED: Rgb = (0.40, 0.42, 0.46);
const GOOD: Rgb = (0.05, 0.45, 0.20);
const BAD: Rgb = (0.70, 0.10, 0.10);
const RULE: Rgb = (0.82, 0.84, 0.87);
const SHADE: Rgb = (0.94, 0.95, 0.96);

/// The WinAnsi bytes of `text`; what the base fonts cannot show is `?`.
pub fn encode(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| match u32::from(c) {
            0x20..=0x7e | 0xa0..=0xff => u8::try_from(u32::from(c)).unwrap_or(b'?'),
            _ => HIGH.iter().find(|(h, _)| *h == c).map_or(b'?', |(_, b)| *b),
        })
        .collect()
}

/// The width of `text` at `size` points.
pub fn width(text: &str, size: f32, bold: bool) -> f32 {
    scaled(units(text, bold), size)
}

/// The width of `text` in thousandths of the font size.
fn units(text: &str, bold: bool) -> u32 {
    let table = if bold { &BOLD } else { &REGULAR };
    encode(text)
        .into_iter()
        .map(|b| u32::from(table[usize::from(b.max(32)) - 32]))
        .sum()
}

fn scaled(units: u32, size: f32) -> f32 {
    units as f32 * size / 1000.0
}

/// `text` cut with an ellipsis to fit `room` points. One pass: each
/// character is one byte of [`encode`], so widths add up, and a cell of
/// a few thousand server names costs no more than reading it.
fn fit(text: &str, size: f32, bold: bool, room: f32) -> String {
    if width(text, size, bold) <= room {
        return text.to_owned();
    }
    let mut used = units("...", bold);
    let mut end = 0;
    let mut one = [0u8; 4];
    for (at, c) in text.char_indices() {
        used += units(c.encode_utf8(&mut one), bold);
        if scaled(used, size) > room {
            break;
        }
        end = at + c.len_utf8();
    }
    format!("{}...", text[..end].trim_end())
}

/// `text` in lines of at most `room` points, broken at spaces.
fn wrap(text: &str, size: f32, bold: bool, room: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let candidate = if line.is_empty() {
            word.to_owned()
        } else {
            format!("{line} {word}")
        };
        if width(&candidate, size, bold) > room && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            line = word.to_owned();
        } else {
            line = candidate;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// A table cell: text, and a color when it says good or bad.
struct Cell(String, Rgb);

impl From<String> for Cell {
    fn from(text: String) -> Self {
        Cell(text, INK)
    }
}

struct Doc {
    pages: Vec<Content>,
    page: Content,
    y: f32,
    language: Language,
}

impl Doc {
    fn new(language: Language) -> Self {
        Self {
            pages: Vec::new(),
            page: Content::new(),
            y: TOP,
            language,
        }
    }

    fn t(&self, en: &'static str, fr: &'static str) -> &'static str {
        self.language.pick(en, fr)
    }

    fn text(&mut self, x: f32, y: f32, size: f32, bold: bool, color: Rgb, text: &str) {
        let font = if bold { Name(b"F2") } else { Name(b"F1") };
        self.page.set_fill_rgb(color.0, color.1, color.2);
        self.page.begin_text();
        self.page.set_font(font, size);
        self.page.next_line(x, y);
        self.page.show(Str(&encode(text)));
        self.page.end_text();
    }

    fn new_page(&mut self) {
        let page = std::mem::replace(&mut self.page, Content::new());
        self.pages.push(page);
        self.y = TOP;
    }

    /// Starts a page when less than `needed` points are left.
    fn room(&mut self, needed: f32) {
        if self.y - needed < BOTTOM {
            self.new_page();
        }
    }

    fn heading(&mut self, text: &str) {
        self.room(60.0);
        self.y -= 22.0;
        self.text(MARGIN, self.y, 13.0, true, INK, text);
        self.y -= 8.0;
    }

    fn paragraph(&mut self, text: &str, color: Rgb) {
        for line in wrap(text, BODY, false, WIDTH - 2.0 * MARGIN) {
            self.room(14.0);
            self.y -= 13.0;
            self.text(MARGIN, self.y, BODY, false, color, &line);
        }
        self.y -= 4.0;
    }

    fn line(&mut self, y: f32) {
        self.page.set_stroke_rgb(RULE.0, RULE.1, RULE.2);
        self.page.set_line_width(0.5);
        self.page.move_to(MARGIN, y);
        self.page.line_to(WIDTH - MARGIN, y);
        self.page.stroke();
    }

    /// A table whose columns take `shares` of the width; the first column
    /// is left-aligned, the others right-aligned except those in `left`.
    fn table(&mut self, shares: &[f32], left: &[usize], header: &[&str], rows: Vec<Vec<Cell>>) {
        let total: f32 = shares.iter().sum();
        let widths: Vec<f32> = shares
            .iter()
            .map(|s| s / total * (WIDTH - 2.0 * MARGIN))
            .collect();
        let row_height = 15.0;
        let draw_header = |doc: &mut Doc| {
            doc.y -= row_height + 2.0;
            doc.page.set_fill_rgb(SHADE.0, SHADE.1, SHADE.2);
            doc.page
                .rect(MARGIN, doc.y - 4.0, WIDTH - 2.0 * MARGIN, row_height);
            doc.page.fill_nonzero();
            doc.cells(
                &widths,
                left,
                header.iter().map(|h| Cell((*h).to_owned(), MUTED)),
                true,
            );
        };
        self.room(row_height * 3.0);
        draw_header(self);
        for row in rows {
            if self.y - row_height < BOTTOM {
                self.new_page();
                draw_header(self);
            }
            self.y -= row_height;
            self.cells(&widths, left, row.into_iter(), false);
            self.line(self.y - 4.5);
        }
        self.y -= 6.0;
    }

    fn cells(
        &mut self,
        widths: &[f32],
        left: &[usize],
        cells: impl Iterator<Item = Cell>,
        bold: bool,
    ) {
        let mut x = MARGIN + 4.0;
        for (i, (cell, w)) in cells.zip(widths).enumerate() {
            let room = w - 8.0;
            let text = fit(&cell.0, BODY, bold, room);
            let at = if i == 0 || left.contains(&i) {
                x
            } else {
                x + room - width(&text, BODY, bold)
            };
            self.text(at, self.y, BODY, bold, cell.1, &text);
            x += w;
        }
    }

    /// Every page with its header (`what`: the title and period) and its
    /// footer: what made it and the page's number.
    fn finish(mut self, what: &str, footer: &str) -> Vec<Content> {
        self.new_page();
        let count = self.pages.len();
        let pages = std::mem::take(&mut self.pages);
        let mut done = Vec::with_capacity(count);
        for (i, page) in pages.into_iter().enumerate() {
            self.page = page;
            brand::header(&mut self, what);
            self.text(MARGIN, 34.0, 8.0, false, MUTED, footer);
            let number = format!("{} {} / {count}", self.t("Page", "Page"), i + 1);
            let at = WIDTH - MARGIN - width(&number, 8.0, false);
            self.text(at, 34.0, 8.0, false, MUTED, &number);
            done.push(std::mem::replace(&mut self.page, Content::new()));
        }
        done
    }
}

const MONTHS_EN: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const MONTHS_FR: [&str; 12] = [
    "janvier",
    "février",
    "mars",
    "avril",
    "mai",
    "juin",
    "juillet",
    "août",
    "septembre",
    "octobre",
    "novembre",
    "décembre",
];

fn month(index: u32, language: Language) -> &'static str {
    let i = usize::try_from(index).unwrap_or(0) % 12;
    match language {
        Language::En => MONTHS_EN[i],
        Language::Fr => MONTHS_FR[i],
    }
}

fn day(at: DateTime<Tz>, language: Language) -> String {
    format!(
        "{} {} {}",
        at.day(),
        month(at.month0(), language),
        at.year()
    )
}

/// The period in words: `October 2026` for a calendar month, `1 October
/// 2026 to 7 October 2026` for whole days, else dates and times.
pub fn period_words(
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    zone: Tz,
    language: Language,
) -> String {
    let (a, b) = (from.with_timezone(&zone), to.with_timezone(&zone));
    let midnight = |d: &DateTime<Tz>| d.time() == NaiveTime::MIN;
    let to_word = language.pick("to", "au");
    if midnight(&a) && midnight(&b) {
        let next_month = if a.month() == 12 {
            (a.year() + 1, 1)
        } else {
            (a.year(), a.month() + 1)
        };
        if a.day() == 1 && b.day() == 1 && (b.year(), b.month()) == next_month {
            let name = month(a.month0(), language);
            return format!("{name} {}", a.year());
        }
        let last = (to - TimeDelta::seconds(1)).with_timezone(&zone);
        if last.date_naive() == a.date_naive() {
            return day(a, language);
        }
        return format!("{} {to_word} {}", day(a, language), day(last, language));
    }
    let clock =
        |d: &DateTime<Tz>| format!("{} {:02}:{:02}", day(*d, language), d.hour(), d.minute());
    format!("{} {to_word} {}", clock(&a), clock(&b))
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

fn availability_cell(value: Option<f64>, target: Option<f64>, language: Language) -> Cell {
    let color = match (value, target) {
        (Some(v), Some(t)) if v + 1e-9 < t => BAD,
        (Some(_), Some(_)) => GOOD,
        _ => INK,
    };
    Cell(words::availability(value, language), color)
}

/// The PDF of `report`, times written in `zone`, with the uptime at the
/// period's end when given (ADR-0172).
pub fn render(report: &Report, uptime: Option<&Uptime>, zone: Tz, language: Language) -> Vec<u8> {
    let mut doc = Doc::new(language);
    let lang = language;
    let d = |n: f64| words::duration(n, lang);
    let title = doc.t("Availability report", "Rapport de disponibilité");
    doc.y -= 10.0;
    doc.text(MARGIN, doc.y, 20.0, true, INK, title);
    doc.y -= 22.0;
    let period = period_words(report.from, report.to, zone, language);
    doc.text(MARGIN, doc.y, 12.0, false, INK, &period);
    if let Some(customer) = &report.customer_name {
        doc.y -= 16.0;
        let line = format!("{} {customer}", doc.t("For", "Pour"));
        doc.text(MARGIN, doc.y, 11.0, false, INK, &line);
    }
    doc.y -= 14.0;
    let zone_line = format!("{} {}", doc.t("Times are in", "Heures en"), zone.name());
    doc.text(MARGIN, doc.y, 8.5, false, MUTED, &zone_line);
    doc.y -= 6.0;

    summary(&mut doc, report);
    if let Some(uptime) = uptime {
        let from = report.from.with_timezone(&zone).date_naive();
        uptime_section::section(&mut doc, uptime, from);
    }

    doc.heading(doc.t(
        "How availability is measured",
        "Comment la disponibilité est mesurée",
    ));
    doc.paragraph(
        doc.t(
            "Down is any time a critical problem was open: a check failing at its critical level, a server offline, or a server unreachable because what it depends on was down. Warnings never count. Planned maintenance, agreed in advance, is shown apart and left out of the calculation. Availability is the time up divided by the time counted.",
            "Une panne est tout moment où un problème critique était ouvert\u{a0}: un contrôle en échec à son seuil critique, un serveur hors ligne, ou un serveur injoignable parce que ce dont il dépend était en panne. Les avertissements ne comptent jamais. La maintenance planifiée, convenue à l’avance, est présentée à part et n’entre pas dans le calcul. La disponibilité est le temps de fonctionnement divisé par le temps compté.",
        ),
        INK,
    );

    if !report.services.is_empty() {
        doc.heading(doc.t("Services", "Services"));
        let rows = report
            .services
            .iter()
            .map(|s| {
                let rule = match s.at_least {
                    Some(n) => format!(
                        "{} {n} {} {}",
                        lang.pick("At least", "Au moins"),
                        lang.pick("of", "sur"),
                        s.members.len()
                    ),
                    None => lang.pick("All members", "Tous les membres").to_owned(),
                };
                let budget = match s.budget_seconds {
                    None => Cell(lang.pick("No target", "Sans objectif").into(), MUTED),
                    Some(b) if b >= 0.0 => Cell(d(b), GOOD),
                    Some(b) => Cell(
                        format!("{} {}", lang.pick("Exceeded by", "Dépassé de"), d(-b)),
                        BAD,
                    ),
                };
                vec![
                    Cell::from(s.name.clone()),
                    Cell::from(rule),
                    Cell::from(s.numbers.outages.to_string()),
                    Cell::from(d(s.numbers.down_seconds)),
                    Cell::from(d(s.numbers.longest_seconds)),
                    budget,
                ]
            })
            .collect();
        let header = [
            lang.pick("Service", "Service"),
            lang.pick("Rule", "Règle"),
            lang.pick("Outages", "Pannes"),
            lang.pick("Down time", "Durée"),
            lang.pick("Longest", "Plus longue"),
            lang.pick("Budget left", "Marge restante"),
        ];
        doc.table(&[2.6, 2.0, 1.0, 1.5, 1.4, 2.7], &[1], &header, rows);
    }

    machines(&mut doc, &report.machines);

    if !report.checks.is_empty() {
        doc.heading(doc.t("Checks with outages", "Contrôles avec des pannes"));
        let rows = report
            .checks
            .iter()
            .map(|c| {
                let name = if c.instance.is_empty() {
                    c.name.clone()
                } else {
                    format!("{} ({})", c.name, c.instance)
                };
                vec![
                    Cell::from(c.server_name.clone()),
                    Cell::from(name),
                    availability_cell(c.numbers.availability, None, lang),
                    Cell::from(c.numbers.outages.to_string()),
                    Cell::from(d(c.numbers.down_seconds)),
                ]
            })
            .collect();
        let header = [
            lang.pick("Server or device", "Serveur ou équipement"),
            lang.pick("Check", "Contrôle"),
            lang.pick("Availability", "Disponibilité"),
            lang.pick("Outages", "Pannes"),
            lang.pick("Down time", "Durée"),
        ];
        doc.table(&[2.5, 3.0, 1.6, 1.1, 1.4], &[1], &header, rows);
        doc.paragraph(
            doc.t(
                "Checks without outages were available all along and are not listed.",
                "Les contrôles sans panne ont été disponibles tout du long et ne sont pas listés.",
            ),
            MUTED,
        );
    }

    let mut outages: Vec<_> = report
        .machines
        .iter()
        .flat_map(|m| m.outage_list.iter())
        .collect();
    outages.sort_by_key(|o| o.start);
    if !outages.is_empty() {
        doc.heading(doc.t("Outages", "Pannes"));
        let more = outages.len().saturating_sub(MAX_OUTAGES);
        let rows = outages
            .iter()
            .take(MAX_OUTAGES)
            .map(|o| {
                let end = if o.ongoing {
                    lang.pick("Ongoing", "En cours").to_owned()
                } else {
                    words::time(o.end, zone)
                };
                vec![
                    Cell::from(words::time(o.start, zone)),
                    Cell::from(end),
                    Cell::from(d(o.seconds)),
                    Cell::from(words::causes(&o.causes, lang)),
                ]
            })
            .collect();
        let header = [
            lang.pick("Start", "Début"),
            lang.pick("End", "Fin"),
            lang.pick("Length", "Durée"),
            lang.pick("What", "Quoi"),
        ];
        doc.table(&[1.6, 1.6, 1.2, 5.0], &[0, 1, 3], &header, rows);
        if more > 0 {
            let text = match lang {
                Language::En => format!("And {more} more, in the CSV file."),
                Language::Fr => format!("Et {more} de plus, dans le fichier CSV."),
            };
            doc.paragraph(&text, MUTED);
        }
    }

    if !report.maintenance.is_empty() {
        doc.heading(doc.t("Planned maintenance", "Maintenance planifiée"));
        let rows = report
            .maintenance
            .iter()
            .take(MAX_OUTAGES)
            .map(|m| {
                vec![
                    Cell::from(words::time(m.start, zone)),
                    Cell::from(words::time(m.end, zone)),
                    Cell::from(m.reason.clone()),
                    Cell::from(m.servers.join(", ")),
                ]
            })
            .collect();
        let header = [
            lang.pick("Start", "Début"),
            lang.pick("End", "Fin"),
            lang.pick("Reason", "Raison"),
            lang.pick("Covered", "Concerne"),
        ];
        doc.table(&[1.6, 1.6, 3.0, 3.2], &[0, 1, 2, 3], &header, rows);
    }

    let footer = format!(
        "MonitorFlow, {} {}",
        doc.t("made on", "établi le"),
        words::time(report.generated_at, zone)
    );
    let what = format!("{title}, {period}");
    let pages = doc.finish(&what, &footer);
    write(pages, title, &period)
}

/// The summary of `report` in a few sentences, for the PDF and the email.
pub fn summary_text(report: &Report, lang: Language) -> String {
    let d = |n: f64| words::duration(n, lang);
    let s = &report.summary;
    let mut text = match lang {
        Language::En => format!(
            "{} watched over this period.",
            plural(
                s.machines,
                "server or device was",
                "servers and devices were"
            )
        ),
        Language::Fr => format!(
            "{} sur cette période.",
            plural(
                s.machines,
                "serveur ou équipement surveillé",
                "serveurs et équipements surveillés"
            )
        ),
    };
    let longest = report
        .machines
        .iter()
        .map(|m| m.numbers.longest_seconds)
        .fold(0.0, f64::max);
    text.push(' ');
    text.push_str(&match (lang, s.outages) {
        (Language::En, 0) => "There was no outage.".to_owned(),
        (Language::Fr, 0) => "Il n’y a eu aucune panne.".to_owned(),
        (Language::En, n) => format!(
            "There {} {}, {} in all; the longest lasted {}.",
            if n == 1 { "was" } else { "were" },
            plural(n, "outage", "outages"),
            d(s.down_seconds),
            d(longest)
        ),
        (Language::Fr, n) => format!(
            "Il y a eu {}, {} en tout\u{a0}; la plus longue a duré {}.",
            plural(n, "panne", "pannes"),
            d(s.down_seconds),
            d(longest)
        ),
    });
    if s.maintenance_seconds > 0.0 {
        text.push(' ');
        text.push_str(&match lang {
            Language::En => format!(
                "Planned maintenance took {}, shown apart and not counted.",
                d(s.maintenance_seconds)
            ),
            Language::Fr => format!(
                "La maintenance planifiée a duré {}, présentée à part et non comptée.",
                d(s.maintenance_seconds)
            ),
        });
    }
    let targets = s.services_met + s.services_missed;
    if targets > 0 {
        text.push(' ');
        text.push_str(&match lang {
            Language::En => format!(
                "{} of {} met {} target.",
                s.services_met,
                plural(targets, "service", "services"),
                if targets == 1 { "its" } else { "their" }
            ),
            Language::Fr => format!(
                "{} sur {} {} son objectif.",
                s.services_met,
                plural(targets, "service", "services"),
                if s.services_met > 1 {
                    "ont atteint"
                } else {
                    "a atteint"
                }
            ),
        });
    }
    text
}

fn summary(doc: &mut Doc, report: &Report) {
    let lang = doc.language;
    doc.heading(doc.t("Summary", "Résumé"));
    let text = summary_text(report, lang);
    doc.paragraph(&text, INK);
    if report.services.is_empty() {
        return;
    }
    let rows = report
        .services
        .iter()
        .map(|service| {
            let result = match service.met {
                Some(true) => Cell(lang.pick("Met", "Atteint").into(), GOOD),
                Some(false) => Cell(lang.pick("Missed", "Manqué").into(), BAD),
                None => Cell(lang.pick("No target", "Sans objectif").into(), MUTED),
            };
            vec![
                Cell::from(service.name.clone()),
                availability_cell(service.numbers.availability, service.target, lang),
                Cell::from(
                    service
                        .target
                        .map(|t| words::target(t, lang))
                        .unwrap_or_else(|| lang.pick("None", "Aucun").into()),
                ),
                result,
            ]
        })
        .collect();
    let header = [
        lang.pick("Service", "Service"),
        lang.pick("Availability", "Disponibilité"),
        lang.pick("Target", "Objectif"),
        lang.pick("Result", "Résultat"),
    ];
    doc.table(&[4.0, 1.8, 1.6, 1.6], &[], &header, rows);
}

fn machines(doc: &mut Doc, machines: &[MachineView]) {
    let lang = doc.language;
    let d = |n: f64| words::duration(n, lang);
    doc.heading(doc.t("Servers and devices", "Serveurs et équipements"));
    if machines.is_empty() {
        doc.paragraph(
            doc.t(
                "Nothing was watched during this period.",
                "Rien n’était surveillé sur cette période.",
            ),
            MUTED,
        );
        return;
    }
    let sites = machines
        .iter()
        .map(|m| m.site_id)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let rows = machines
        .iter()
        .map(|m| {
            let mut row = vec![Cell::from(m.name.clone())];
            if sites > 1 {
                row.push(Cell::from(m.site_name.clone()));
            }
            row.extend([
                availability_cell(m.numbers.availability, None, lang),
                Cell::from(m.numbers.outages.to_string()),
                Cell::from(d(m.numbers.down_seconds)),
                Cell::from(d(m.numbers.longest_seconds)),
                if m.numbers.maintenance_seconds > 0.0 {
                    Cell::from(d(m.numbers.maintenance_seconds))
                } else {
                    Cell(lang.pick("None", "Aucune").into(), MUTED)
                },
            ]);
            row
        })
        .collect();
    let mut header = vec![lang.pick("Server or device", "Serveur ou équipement")];
    let mut shares = vec![3.0];
    if sites > 1 {
        header.push(lang.pick("Site", "Site"));
        shares.push(1.8);
    }
    header.extend([
        lang.pick("Availability", "Disponibilité"),
        lang.pick("Outages", "Pannes"),
        lang.pick("Down time", "Durée"),
        lang.pick("Longest", "Plus longue"),
        lang.pick("Maintenance", "Maintenance"),
    ]);
    shares.extend([1.6, 1.1, 1.4, 1.4, 1.5]);
    let left: Vec<usize> = if sites > 1 { vec![1] } else { vec![] };
    doc.table(&shares, &left, &header, rows);
}

/// The document of `pages`, with both fonts on each.
fn write(pages: Vec<Content>, title: &str, period: &str) -> Vec<u8> {
    let mut pdf = Pdf::new();
    let catalog = Ref::new(1);
    let tree = Ref::new(2);
    let regular = Ref::new(3);
    let bold = Ref::new(4);
    let info = Ref::new(5);
    let first = 6;
    let ids: Vec<(Ref, Ref)> = (0..pages.len())
        .map(|i| {
            let n = first + 2 * i32::try_from(i).unwrap_or(0);
            (Ref::new(n), Ref::new(n + 1))
        })
        .collect();
    pdf.catalog(catalog).pages(tree);
    pdf.pages(tree)
        .kids(ids.iter().map(|(page, _)| *page))
        .count(i32::try_from(ids.len()).unwrap_or(0));
    for (font, base) in [(regular, &b"Helvetica"[..]), (bold, &b"Helvetica-Bold"[..])] {
        pdf.type1_font(font)
            .base_font(Name(base))
            .encoding_predefined(Name(b"WinAnsiEncoding"));
    }
    pdf.document_info(info)
        .title(TextStr(&format!("{title}, {period}")))
        .producer(TextStr("MonitorFlow"));
    for ((page_id, content_id), content) in ids.into_iter().zip(pages) {
        let mut page = pdf.page(page_id);
        page.media_box(Rect::new(0.0, 0.0, WIDTH, HEIGHT));
        page.parent(tree);
        page.contents(content_id);
        let mut resources = page.resources();
        resources
            .fonts()
            .pair(Name(b"F1"), regular)
            .pair(Name(b"F2"), bold);
        resources.finish();
        page.finish();
        pdf.stream(content_id, &content.finish());
    }
    pdf.finish()
}

mod brand;
mod uptime_section;

#[cfg(test)]
mod tests;
