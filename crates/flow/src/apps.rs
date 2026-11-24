//! Applications deduced from ports. A flow's application is its protocol
//! and the port that names a service: the one of the pair this list
//! knows, else the lower one, since clients pick high ephemeral ports.
//! Keys are `tcp/443`, or the protocol alone when it has no ports.

/// Names of IP protocols, by number.
const PROTOCOLS: [(u8, &str); 10] = [
    (1, "icmp"),
    (2, "igmp"),
    (6, "tcp"),
    (17, "udp"),
    (47, "gre"),
    (50, "esp"),
    (51, "ah"),
    (58, "icmpv6"),
    (89, "ospf"),
    (132, "sctp"),
];

/// Services people recognize, by key. Names are product and protocol
/// names, the same in every language.
const APPS: [(&str, &str); 64] = [
    ("tcp/20", "FTP data"),
    ("tcp/21", "FTP"),
    ("tcp/22", "SSH"),
    ("tcp/23", "Telnet"),
    ("tcp/25", "SMTP"),
    ("tcp/53", "DNS"),
    ("udp/53", "DNS"),
    ("udp/67", "DHCP"),
    ("udp/68", "DHCP"),
    ("udp/69", "TFTP"),
    ("tcp/80", "HTTP"),
    ("tcp/88", "Kerberos"),
    ("udp/88", "Kerberos"),
    ("tcp/110", "POP3"),
    ("udp/123", "NTP"),
    ("udp/137", "NetBIOS"),
    ("udp/138", "NetBIOS"),
    ("tcp/139", "NetBIOS"),
    ("tcp/143", "IMAP"),
    ("udp/161", "SNMP"),
    ("udp/162", "SNMP traps"),
    ("tcp/179", "BGP"),
    ("tcp/389", "LDAP"),
    ("udp/389", "LDAP"),
    ("tcp/443", "HTTPS"),
    ("udp/443", "QUIC"),
    ("tcp/445", "SMB"),
    ("tcp/465", "SMTPS"),
    ("udp/500", "IKE"),
    ("udp/514", "Syslog"),
    ("tcp/587", "SMTP submission"),
    ("tcp/631", "IPP"),
    ("tcp/636", "LDAPS"),
    ("tcp/853", "DNS over TLS"),
    ("tcp/873", "rsync"),
    ("tcp/993", "IMAPS"),
    ("tcp/995", "POP3S"),
    ("udp/1194", "OpenVPN"),
    ("tcp/1433", "SQL Server"),
    ("udp/1812", "RADIUS"),
    ("udp/1813", "RADIUS accounting"),
    ("tcp/1883", "MQTT"),
    ("tcp/2049", "NFS"),
    ("udp/2055", "NetFlow"),
    ("tcp/3268", "Global catalog"),
    ("tcp/3306", "MySQL"),
    ("tcp/3389", "RDP"),
    ("udp/3389", "RDP"),
    ("udp/3478", "STUN"),
    ("udp/4500", "IPsec NAT-T"),
    ("udp/4739", "IPFIX"),
    ("tcp/5060", "SIP"),
    ("udp/5060", "SIP"),
    ("tcp/5061", "SIPS"),
    ("tcp/5432", "PostgreSQL"),
    ("tcp/5985", "WinRM"),
    ("tcp/5986", "WinRM over HTTPS"),
    ("tcp/6379", "Redis"),
    ("udp/6343", "sFlow"),
    ("tcp/8080", "HTTP alternate"),
    ("tcp/8443", "HTTPS alternate"),
    ("tcp/8883", "MQTTS"),
    ("tcp/9100", "Printing"),
    ("tcp/27017", "MongoDB"),
];

/// The protocol's key: its name, or its number.
pub fn protocol_key(protocol: u8) -> String {
    PROTOCOLS
        .iter()
        .find(|(n, _)| *n == protocol)
        .map_or_else(|| protocol.to_string(), |(_, name)| (*name).to_string())
}

/// The application of a flow: `tcp/443`, or the protocol's key when it
/// has no ports.
pub fn app_key(protocol: u8, src_port: u16, dst_port: u16) -> String {
    let name = protocol_key(protocol);
    if !matches!(protocol, 6 | 17 | 132) || (src_port == 0 && dst_port == 0) {
        return name;
    }
    let known = |port: u16| {
        let key = format!("{name}/{port}");
        app_name(&key).is_some()
    };
    let port = match (known(src_port), known(dst_port)) {
        (true, false) => src_port,
        (false, true) => dst_port,
        _ if src_port == 0 => dst_port,
        _ if dst_port == 0 => src_port,
        _ => src_port.min(dst_port),
    };
    format!("{name}/{port}")
}

/// The name of a known application key.
pub fn app_name(key: &str) -> Option<&'static str> {
    APPS.iter().find(|(k, _)| *k == key).map(|(_, name)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_port_that_names_a_service() {
        assert_eq!(app_key(6, 51_234, 443), "tcp/443");
        assert_eq!(app_key(6, 443, 51_234), "tcp/443");
        assert_eq!(app_key(17, 53, 123), "udp/53", "both known: the lower");
        assert_eq!(app_key(6, 40_000, 30_000), "tcp/30000");
        assert_eq!(app_key(1, 0, 0), "icmp");
        assert_eq!(app_key(250, 1, 2), "250");
        assert_eq!(app_name("tcp/443"), Some("HTTPS"));
        assert_eq!(app_name("tcp/30000"), None);
        let mut keys: Vec<_> = APPS.iter().map(|(k, _)| k).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), APPS.len(), "no key twice");
    }
}
