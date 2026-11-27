//! A named instance's port, from SQL Server Browser (MS-SQLR): one UDP
//! datagram to port 1434 naming the instance (`CLNT_UCAST_INST`), one
//! back listing its protocols (`SVR_RESP`), `tcp` with its port among
//! them. Asked at every collection, as an instance on a dynamic port
//! takes another one when it restarts.

use std::time::Duration;

use tokio::net::UdpSocket;

use super::TdsError;

/// SQL Server Browser's port.
pub const PORT: u16 = 1434;
/// Longest instance name SQL Server takes.
pub const MAX_INSTANCE: usize = 16;
/// Time for one answer before the request is sent again; UDP may lose
/// either.
const WAIT: Duration = Duration::from_millis(1500);
const ATTEMPTS: usize = 3;
/// An answer for one instance is at most 1,024 bytes after its header.
const MAX_ANSWER: usize = 3 + 1024;

/// Whether `name` is an instance name SQL Server accepts: 1 to 16
/// letters, digits, `_`, `$` or `#`, starting with a letter or `_`.
pub fn valid_instance(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.len() <= MAX_INSTANCE
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '#'))
}

/// `CLNT_UCAST_INST`: 0x04, then the name, NUL-terminated.
pub(crate) fn request(instance: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(instance.len() + 2);
    out.push(0x04);
    out.extend_from_slice(instance.as_bytes());
    out.push(0);
    out
}

/// The TCP port of `instance` in `SVR_RESP`: 0x05, a little-endian
/// length, then `Key;value;` pairs, each instance ending with `;;`.
pub(crate) fn parse_answer(answer: &[u8], instance: &str) -> Result<u16, TdsError> {
    let bad = || TdsError::Browser("SQL Server Browser sent an answer it should not".into());
    let (&kind, rest) = answer.split_first().ok_or_else(bad)?;
    let size = rest.get(..2).ok_or_else(bad)?;
    let size = usize::from(u16::from_le_bytes([size[0], size[1]]));
    if kind != 0x05 {
        return Err(bad());
    }
    let data = rest.get(2..2 + size).ok_or_else(bad)?;
    // Latin-1 at worst: the keys and an instance's name are ASCII.
    let text: String = data.iter().map(|b| char::from(*b)).collect();
    for entry in text.split(";;").filter(|e| !e.is_empty()) {
        let fields: Vec<&str> = entry.split(';').collect();
        let value = |key: &str| {
            fields
                .chunks(2)
                .find(|pair| pair[0].eq_ignore_ascii_case(key))
                .and_then(|pair| pair.get(1).copied())
        };
        if !value("InstanceName").is_some_and(|name| name.eq_ignore_ascii_case(instance)) {
            continue;
        }
        return match value("tcp") {
            Some(port) => port
                .trim()
                .parse::<u16>()
                .ok()
                .filter(|p| *p > 0)
                .ok_or_else(bad),
            None => Err(TdsError::Browser(format!(
                "the instance {instance} does not listen on TCP: turn TCP/IP on in SQL Server Configuration Manager"
            ))),
        };
    }
    Err(TdsError::Browser(format!(
        "SQL Server Browser does not know the instance {instance}"
    )))
}

/// Asks SQL Server Browser on `host`, at `port` ([`PORT`] but in tests),
/// for the TCP port of `instance`.
pub async fn port_of(host: &str, port: u16, instance: &str) -> Result<u16, TdsError> {
    let address = tokio::net::lookup_host((host, port))
        .await?
        .next()
        .ok_or_else(|| TdsError::Browser(format!("{host} has no address")))?;
    let local: std::net::SocketAddr = if address.is_ipv4() {
        (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let socket = UdpSocket::bind(local).await?;
    // Connected: datagrams from anywhere else are dropped.
    socket.connect(address).await?;
    let ask = request(instance);
    let mut answer = vec![0u8; MAX_ANSWER];
    for _ in 0..ATTEMPTS {
        socket.send(&ask).await?;
        match tokio::time::timeout(WAIT, socket.recv(&mut answer)).await {
            Ok(Ok(n)) => return parse_answer(&answer[..n], instance),
            // A port closed answers with ICMP, seen here as refused (as
            // reset on Windows).
            Ok(Err(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::ConnectionReset
                ) =>
            {
                break
            }
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {}
        }
    }
    Err(TdsError::Browser(format!(
        "SQL Server Browser did not answer on UDP {port} for the instance {instance}: \
         the service is stopped, the instance does not exist, or a firewall drops UDP {port}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(text: &str) -> Vec<u8> {
        let mut out = vec![0x05];
        out.extend_from_slice(&u16::try_from(text.len()).unwrap().to_le_bytes());
        out.extend_from_slice(text.as_bytes());
        out
    }

    #[test]
    fn instance_names() {
        assert!(valid_instance("SQLEXPRESS"));
        assert!(valid_instance("_sales$2#"));
        assert!(!valid_instance(""));
        assert!(!valid_instance("1st"));
        assert!(!valid_instance("a-b"));
        assert!(!valid_instance("a\\b"));
        assert!(!valid_instance("SEVENTEEN_LETTERS"));
    }

    #[test]
    fn the_request_names_the_instance() {
        assert_eq!(request("SALES"), b"\x04SALES\x00");
    }

    #[test]
    fn the_port_of_the_instance_named() {
        let both = "ServerName;DB1;InstanceName;SQLEXPRESS;IsClustered;No;Version;16.0.1000.6;tcp;49712;np;\\\\DB1\\pipe\\MSSQL$SQLEXPRESS\\sql\\query;;\
ServerName;DB1;InstanceName;SALES;IsClustered;No;Version;15.0.2000.5;tcp;50123;;";
        assert_eq!(parse_answer(&answer(both), "sales").unwrap(), 50123);
        assert_eq!(parse_answer(&answer(both), "SQLEXPRESS").unwrap(), 49712);
        let error = parse_answer(&answer(both), "OTHER").unwrap_err();
        assert!(error.to_string().contains("does not know"), "{error}");
        let pipes = "ServerName;DB1;InstanceName;SALES;IsClustered;No;Version;15.0.2000.5;np;\\\\DB1\\pipe\\sql\\query;;";
        let error = parse_answer(&answer(pipes), "SALES").unwrap_err();
        assert!(error.to_string().contains("TCP"), "{error}");
    }

    #[test]
    fn odd_answers_are_refused() {
        assert!(parse_answer(&[], "A").is_err());
        assert!(parse_answer(&[0x05, 10, 0, b'x'], "A").is_err());
        assert!(parse_answer(&[0x04, 0, 0], "A").is_err());
        let zero = answer("ServerName;DB1;InstanceName;A;tcp;0;;");
        assert!(parse_answer(&zero, "A").is_err());
        let words = answer("ServerName;DB1;InstanceName;A;tcp;many;;");
        assert!(parse_answer(&words, "A").is_err());
    }

    /// Over UDP, against a Browser of the test's own: the first request
    /// is dropped, so the second one is answered.
    #[tokio::test]
    async fn asked_over_udp_and_asked_again() {
        let browser = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = browser.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let mut buf = [0u8; 64];
            let (n, _) = browser.recv_from(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"\x04SALES\x00");
            let (_, from) = browser.recv_from(&mut buf).await.unwrap();
            let text = "ServerName;DB1;InstanceName;SALES;IsClustered;No;tcp;50123;;";
            browser.send_to(&answer(text), from).await.unwrap();
        });
        assert_eq!(port_of("127.0.0.1", port, "SALES").await.unwrap(), 50123);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn no_browser_says_so() {
        // A port just freed: nothing listens there.
        let port = UdpSocket::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let error = port_of("127.0.0.1", port, "SALES").await.unwrap_err();
        assert!(matches!(error, TdsError::Browser(_)), "{error:?} {error}");
    }
}
