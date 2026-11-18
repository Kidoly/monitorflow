//! Small parsers shared by readers: durations, names and sizes as other
//! tools write them.

/// Seconds of a duration such as `30s`, `5m`, `1h30m`, `2d`, `1w` or a
/// plain number of seconds. `None` for anything else, such as a macro.
pub fn seconds(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(n) = text.parse::<u64>() {
        return Some(n);
    }
    let mut total: u64 = 0;
    let mut number = String::new();
    let mut chars = text.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() || c == '.' {
            number.push(c);
            continue;
        }
        let unit = match c {
            's' => 1.0,
            'm' if chars.peek() == Some(&'s') => {
                chars.next();
                0.001
            }
            'm' => 60.0,
            'h' => 3600.0,
            'd' => 86_400.0,
            'w' => 604_800.0,
            'y' => 31_536_000.0,
            _ => return None,
        };
        let value: f64 = number.parse().ok()?;
        number.clear();
        total = total.checked_add((value * unit).round() as u64)?;
        any = true;
    }
    (any && number.is_empty()).then_some(total)
}

/// A value with an optional size or time suffix as monitoring tools write
/// thresholds: `90`, `0.5`, `10G`, `512M`, `5m`. Sizes are in bytes.
pub fn number(text: &str) -> Option<f64> {
    let text = text.trim();
    if let Ok(n) = text.parse::<f64>() {
        return n.is_finite().then_some(n);
    }
    let (digits, suffix) = text.split_at(text.find(|c: char| c.is_ascii_alphabetic())?);
    let value: f64 = digits.trim().parse().ok()?;
    let factor = match suffix {
        "K" | "k" => 1024.0,
        "M" => 1024.0 * 1024.0,
        "G" => 1024.0_f64.powi(3),
        "T" => 1024.0_f64.powi(4),
        _ => return seconds(text).map(|s| s as f64),
    };
    Some(value * factor)
}

/// The host part of an address `host:port`, `[v6]:port`, a URL or a bare
/// host.
pub fn host_of(address: &str) -> String {
    let rest = address
        .split_once("://")
        .map_or(address, |(_, rest)| rest)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let rest = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    if let Some(v6) = rest.strip_prefix('[') {
        return v6.split(']').next().unwrap_or_default().to_owned();
    }
    match rest.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) && !host.contains(':') => {
            host.to_owned()
        }
        _ => rest.to_owned(),
    }
}

/// The port of an address or URL: given, or the scheme's default.
pub fn port_of(address: &str) -> Option<u16> {
    let (scheme, rest) = address
        .split_once("://")
        .map_or((None, address), |(s, r)| (Some(s), r));
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let port = match authority.strip_prefix('[') {
        Some(v6) => v6.split_once("]:").map(|(_, p)| p),
        None => authority
            .rsplit_once(':')
            .filter(|(h, _)| !h.contains(':'))
            .map(|(_, p)| p),
    };
    match port {
        Some(p) => p.parse().ok(),
        None => match scheme.map(str::to_ascii_lowercase).as_deref() {
            Some("https") => Some(443),
            Some("http") => Some(80),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(seconds("30s"), Some(30));
        assert_eq!(seconds("5m"), Some(300));
        assert_eq!(seconds("1h30m"), Some(5400));
        assert_eq!(seconds("2d"), Some(172_800));
        assert_eq!(seconds("1w"), Some(604_800));
        assert_eq!(seconds("60"), Some(60));
        assert_eq!(seconds("500ms"), Some(1));
        assert_eq!(seconds("1.5m"), Some(90));
        assert_eq!(seconds("{$DELAY}"), None);
        assert_eq!(seconds(""), None);
        assert_eq!(seconds("5"), Some(5));
        assert_eq!(seconds("5x"), None);
    }

    #[test]
    fn numbers() {
        assert_eq!(number("90"), Some(90.0));
        assert_eq!(number("0.5"), Some(0.5));
        assert_eq!(number("10G"), Some(10.0 * 1024.0 * 1024.0 * 1024.0));
        assert_eq!(number("5m"), Some(300.0));
        assert_eq!(number("{$X}"), None);
    }

    #[test]
    fn addresses() {
        assert_eq!(host_of("web-01:9100"), "web-01");
        assert_eq!(host_of("https://shop.example/health"), "shop.example");
        assert_eq!(host_of("[::1]:9100"), "::1");
        assert_eq!(host_of("10.0.0.5"), "10.0.0.5");
        assert_eq!(port_of("web-01:9100"), Some(9100));
        assert_eq!(port_of("https://shop.example/health"), Some(443));
        assert_eq!(port_of("http://shop.example:8080/"), Some(8080));
        assert_eq!(port_of("[::1]:22"), Some(22));
        assert_eq!(port_of("shop.example"), None);
    }
}

/// The group of MonitorFlow's SNMP checks (ADR-0149) that an OID or an
/// object name of the common MIBs belongs to: `system`, `interfaces`,
/// `host`, `ups`, `temperature` or `printer`; `None` for other objects.
pub fn snmp_group(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    let numeric = |prefix: &str| {
        lower
            .split(|c: char| !(c.is_ascii_digit() || c == '.'))
            .any(|part| part.trim_start_matches('.').starts_with(prefix))
    };
    let named = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    if numeric("1.3.6.1.2.1.2.2.1")
        || numeric("1.3.6.1.2.1.31.1.1.1")
        || named(&[
            "ifhcin",
            "ifhcout",
            "ifin",
            "ifout",
            "ifoperstatus",
            "ifadminstatus",
            "ifspeed",
            "ifhighspeed",
            "ifdescr",
            "ifalias",
            "ifname",
        ])
    {
        Some("interfaces")
    } else if numeric("1.3.6.1.2.1.25.")
        || named(&["hrprocessorload", "hrstorage", "hrsystem", "hrdevice"])
    {
        Some("host")
    } else if numeric("1.3.6.1.2.1.33.")
        || named(&["upsbattery", "upsestimated", "upsoutput", "upsinput"])
    {
        Some("ups")
    } else if numeric("1.3.6.1.2.1.99.") || named(&["entphysensor"]) {
        Some("temperature")
    } else if numeric("1.3.6.1.2.1.43.") || named(&["prtmarker"]) {
        Some("printer")
    } else if numeric("1.3.6.1.2.1.1.")
        || named(&[
            "sysuptime",
            "sysdescr",
            "sysname",
            "syslocation",
            "syscontact",
            "sysobjectid",
        ])
    {
        Some("system")
    } else {
        None
    }
}

#[cfg(test)]
mod snmp_tests {
    use super::snmp_group;

    #[test]
    fn oids_and_names_find_their_group() {
        for (text, group) in [
            ("1.3.6.1.2.1.31.1.1.1.6.{#SNMPINDEX}", Some("interfaces")),
            ("walk[1.3.6.1.2.1.2.2.1.8]", Some("interfaces")),
            ("net.if.in[ifHCInOctets.{#SNMPINDEX}]", Some("interfaces")),
            ("get[1.3.6.1.2.1.1.3.0]", Some("system")),
            ("system.uptime[sysUpTime.0]", Some("system")),
            ("1.3.6.1.2.1.25.3.3.1.2.196608", Some("host")),
            ("upsBatteryStatus.0", Some("ups")),
            (".1.3.6.1.2.1.33.1.2.4.0", Some("ups")),
            ("1.3.6.1.2.1.99.1.1.1.4.21", Some("temperature")),
            ("prtMarkerSuppliesLevel.1.1", Some("printer")),
            ("1.3.6.1.4.1.9.9.13.1.3.1.3", None),
            ("1.3.6.1.2.1.10.7.2", None),
        ] {
            assert_eq!(snmp_group(text), group, "{text}");
        }
    }
}
