//! A GET over a Unix socket, for the Docker Engine API: HTTP/1.0, so the
//! engine closes the connection after the answer, and a bounded body.

/// Largest answer read: the list of a few hundred containers fits.
pub const MAX_BODY: usize = 16 * 1024 * 1024;

/// An answer's status and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Reads `raw`, an HTTP/1.x answer as received until the connection
/// closed.
pub fn parse(raw: &[u8]) -> Result<Answer, String> {
    let end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("an answer without its headers")?;
    let head = String::from_utf8_lossy(&raw[..end]);
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|l| {
            let mut parts = l.split(' ');
            parts.next().filter(|v| v.starts_with("HTTP/1."))?;
            parts.next()?.parse::<u16>().ok()
        })
        .ok_or("an answer that is not HTTP")?;
    let chunked = lines.any(|l| {
        l.split_once(':').is_some_and(|(k, v)| {
            k.trim().eq_ignore_ascii_case("transfer-encoding")
                && v.trim().eq_ignore_ascii_case("chunked")
        })
    });
    let body = &raw[end + 4..];
    let body = if chunked { unchunk(body)? } else { body.to_vec() };
    Ok(Answer { status, body })
}

/// The body of a chunked answer.
fn unchunk(mut body: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("a chunk without its size")?;
        let size = std::str::from_utf8(&body[..line_end])
            .ok()
            .and_then(|s| usize::from_str_radix(s.split(';').next()?.trim(), 16).ok())
            .ok_or("a bad chunk size")?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if body.len() < size {
            return Err("a chunk cut short".into());
        }
        out.extend_from_slice(&body[..size]);
        body = body.get(size + 2..).unwrap_or_default();
    }
}

#[cfg(unix)]
pub async fn get(socket: &std::path::Path, path: &str) -> std::io::Result<Answer> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::UnixStream::connect(socket).await?;
    stream
        .write_all(format!("GET {path} HTTP/1.0\r\nHost: docker\r\n\r\n").as_bytes())
        .await?;
    let mut raw = Vec::new();
    (&mut stream)
        .take(MAX_BODY as u64 + 64 * 1024)
        .read_to_end(&mut raw)
        .await?;
    parse(&raw).map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_and_chunked_answers() {
        let plain = parse(b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n[1]").unwrap();
        assert_eq!(plain.status, 200);
        assert_eq!(plain.body, b"[1]");
        let chunked =
            parse(b"HTTP/1.1 404 Not Found\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n")
                .unwrap();
        assert_eq!(chunked.status, 404);
        assert_eq!(chunked.body, b"abcde");
        assert!(parse(b"SSH-2.0-OpenSSH\r\n\r\n").is_err());
        assert!(parse(b"HTTP/1.1 200 OK\r\n").is_err());
        assert!(parse(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nff\r\nabc").is_err());
    }
}
