//! The words of reports, in English and French (ADR-0155): durations,
//! percentages, dates in the report's time zone, and what caused an
//! outage. The PDF and the CSV say the same things.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;

use super::report::Cause;

/// The language of a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    En,
    Fr,
}

impl Language {
    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "en" => Some(Self::En),
            "fr" => Some(Self::Fr),
            _ => None,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Fr => "fr",
        }
    }

    /// `en` or `fr` of a pair.
    pub fn pick(self, en: &'static str, fr: &'static str) -> &'static str {
        match self {
            Self::En => en,
            Self::Fr => fr,
        }
    }
}

/// A length of time in words: `45 s`, `12 min`, `1 h 12 min`, `3 d 4 h`
/// (`3 j 4 h` in French), to the second below a minute.
pub fn duration(seconds: f64, language: Language) -> String {
    let total = seconds.max(0.0).round() as i64;
    let (days, hours, minutes, secs) = (
        total / 86_400,
        total % 86_400 / 3600,
        total % 3600 / 60,
        total % 60,
    );
    let day = language.pick("d", "j");
    match (days, hours, minutes) {
        (0, 0, 0) => format!("{secs} s"),
        (0, 0, m) if secs == 0 => format!("{m} min"),
        (0, 0, m) => format!("{m} min {secs} s"),
        (0, h, 0) => format!("{h} h"),
        (0, h, m) => format!("{h} h {m} min"),
        (d, 0, _) => format!("{d} {day}"),
        (d, h, _) => format!("{d} {day} {h} h"),
    }
}

/// A percentage with three decimals, never rounded up to 100: 99.9996 is
/// `99.999 %`. French writes a decimal comma and a no-break space.
pub fn percent(value: f64, language: Language) -> String {
    let floored = (value * 1000.0).floor() / 1000.0;
    let text = format!("{floored:.3}");
    match language {
        Language::En => format!("{text} %"),
        Language::Fr => format!("{}\u{a0}%", text.replace('.', ",")),
    }
}

/// An SLA target as it was set: `99.9 %`, `99,95 %`.
pub fn target(value: f64, language: Language) -> String {
    let text = format!("{value:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    match language {
        Language::En => format!("{text} %"),
        Language::Fr => format!("{}\u{a0}%", text.replace('.', ",")),
    }
}

/// An availability, or "Not measured" when nothing was counted.
pub fn availability(value: Option<f64>, language: Language) -> String {
    match value {
        Some(v) => percent(v, language),
        None => language.pick("Not measured", "Non mesurée").into(),
    }
}

/// A date and time in `zone`, like `2026-10-04 14:05`.
pub fn time(at: DateTime<Utc>, zone: Tz) -> String {
    at.with_timezone(&zone).format("%Y-%m-%d %H:%M").to_string()
}

/// What a cause was, in a few words: "web-1: check disk (/)", "web-1:
/// offline", "web-1: unreachable, relay-paris down".
pub fn cause(cause: &Cause, language: Language) -> String {
    let what = match cause.kind.as_str() {
        "offline" => language.pick("offline", "hors ligne").to_owned(),
        "unreachable" => match &cause.via {
            Some(via) => format!(
                "{} {via} {}",
                language.pick("unreachable,", "injoignable,"),
                language.pick("down", "en panne")
            ),
            None => language.pick("unreachable", "injoignable").to_owned(),
        },
        _ => match &cause.check {
            Some(check) if cause.instance.is_empty() => {
                format!("{} {check}", language.pick("check", "contrôle"))
            }
            Some(check) => format!(
                "{} {check} ({})",
                language.pick("check", "contrôle"),
                cause.instance
            ),
            None => return cause.server_name.clone(),
        },
    };
    format!(
        "{}{}{what}",
        cause.server_name,
        language.pick(": ", "\u{a0}: ")
    )
}

/// The causes of an outage, joined.
pub fn causes(causes: &[Cause], language: Language) -> String {
    causes
        .iter()
        .map(|c| cause(c, language))
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    #[test]
    fn durations_in_words() {
        let en = Language::En;
        assert_eq!(duration(45.0, en), "45 s");
        assert_eq!(duration(720.0, en), "12 min");
        assert_eq!(duration(750.0, en), "12 min 30 s");
        assert_eq!(duration(4320.0, en), "1 h 12 min");
        assert_eq!(duration(7200.0, en), "2 h");
        assert_eq!(
            duration(3.0 * 86_400.0 + 4.0 * 3600.0, Language::Fr),
            "3 j 4 h"
        );
    }

    #[test]
    fn percentages_never_round_up_to_full() {
        assert_eq!(percent(99.9996, Language::En), "99.999 %");
        assert_eq!(percent(100.0, Language::En), "100.000 %");
        assert_eq!(percent(99.95, Language::Fr), "99,950\u{a0}%");
        assert_eq!(availability(None, Language::Fr), "Non mesurée");
        assert_eq!(target(99.9, Language::En), "99.9 %");
        assert_eq!(target(100.0, Language::Fr), "100\u{a0}%");
    }

    #[test]
    fn causes_in_words() {
        let mut c = Cause {
            kind: "check".into(),
            server_id: Uuid::nil(),
            server_name: "web-1".into(),
            check: Some("disk".into()),
            instance: "/".into(),
            via: None,
        };
        assert_eq!(cause(&c, Language::En), "web-1: check disk (/)");
        c.kind = "unreachable".into();
        c.via = Some("relay-paris".into());
        assert_eq!(
            cause(&c, Language::Fr),
            "web-1\u{a0}: injoignable, relay-paris en panne"
        );
        c.kind = "offline".into();
        assert_eq!(cause(&c, Language::En), "web-1: offline");
    }

    #[test]
    fn times_in_the_zone() {
        let at = DateTime::parse_from_rfc3339("2026-10-31T23:30:00Z")
            .unwrap()
            .to_utc();
        assert_eq!(time(at, chrono_tz::Europe::Paris), "2026-11-01 00:30");
    }
}
