//! The little of the Redis protocol (RESP2) a check needs: commands as
//! arrays of bulk strings, and simple strings, errors and bulk strings
//! back, each bounded.

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Longest reply read: `INFO` is a few kilobytes.
pub const MAX_REPLY: usize = 1024 * 1024;
/// Longest line of a reply header.
const MAX_LINE: usize = 4096;

/// One reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Simple(String),
    Error(String),
    Bulk(Option<String>),
    Integer(i64),
}

#[derive(Debug, thiserror::Error)]
pub enum RespError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Protocol(String),
}

/// Writes `args` as one command.
pub async fn send<W: AsyncWrite + Unpin>(w: &mut W, args: &[&str]) -> Result<(), RespError> {
    let mut out = format!("*{}\r\n", args.len()).into_bytes();
    for arg in args {
        out.extend_from_slice(format!("${}\r\n", arg.len()).as_bytes());
        out.extend_from_slice(arg.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    w.write_all(&out).await?;
    w.flush().await?;
    Ok(())
}

/// Reads one reply.
pub async fn read<R: AsyncBufRead + Unpin>(r: &mut R) -> Result<Reply, RespError> {
    let line = read_line(r).await?;
    let (kind, rest) = line.split_at(line.len().min(1));
    match kind {
        "+" => Ok(Reply::Simple(rest.to_owned())),
        "-" => Ok(Reply::Error(rest.to_owned())),
        ":" => rest
            .parse()
            .map(Reply::Integer)
            .map_err(|_| RespError::Protocol("a bad integer reply".into())),
        "$" => {
            let len: i64 = rest
                .parse()
                .map_err(|_| RespError::Protocol("a bad bulk length".into()))?;
            if len < 0 {
                return Ok(Reply::Bulk(None));
            }
            let len = usize::try_from(len).unwrap_or(usize::MAX);
            if len > MAX_REPLY {
                return Err(RespError::Protocol(format!(
                    "a reply of {len} bytes, more than {MAX_REPLY}"
                )));
            }
            let mut body = vec![0; len + 2];
            r.read_exact(&mut body).await?;
            if !body.ends_with(b"\r\n") {
                return Err(RespError::Protocol("a bulk reply without its end".into()));
            }
            body.truncate(len);
            Ok(Reply::Bulk(Some(String::from_utf8_lossy(&body).into_owned())))
        }
        _ => Err(RespError::Protocol(
            "the server does not speak the Redis protocol".into(),
        )),
    }
}

async fn read_line<R: AsyncBufRead + Unpin>(r: &mut R) -> Result<String, RespError> {
    let mut line = Vec::new();
    let mut limited = (&mut *r).take(MAX_LINE as u64);
    limited.read_until(b'\n', &mut line).await?;
    if line.is_empty() {
        return Err(RespError::Io(std::io::ErrorKind::UnexpectedEof.into()));
    }
    if !line.ends_with(b"\r\n") {
        return Err(RespError::Protocol("a reply line too long or cut".into()));
    }
    line.truncate(line.len() - 2);
    Ok(String::from_utf8_lossy(&line).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn parse(bytes: &[u8]) -> Result<Reply, RespError> {
        let mut r = tokio::io::BufReader::new(bytes);
        read(&mut r).await
    }

    #[tokio::test]
    async fn replies() {
        assert_eq!(parse(b"+OK\r\n").await.unwrap(), Reply::Simple("OK".into()));
        assert_eq!(
            parse(b"-WRONGPASS bad\r\n").await.unwrap(),
            Reply::Error("WRONGPASS bad".into())
        );
        assert_eq!(parse(b":42\r\n").await.unwrap(), Reply::Integer(42));
        assert_eq!(
            parse(b"$5\r\nhello\r\n").await.unwrap(),
            Reply::Bulk(Some("hello".into()))
        );
        assert_eq!(parse(b"$-1\r\n").await.unwrap(), Reply::Bulk(None));
        assert!(parse(b"HTTP/1.1 400\r\n").await.is_err());
        assert!(parse(b"$99999999999\r\n").await.is_err());
        assert!(parse(b"$5\r\nhel").await.is_err());
        assert!(parse(b"").await.is_err());
    }

    #[tokio::test]
    async fn commands() {
        let mut out = Vec::new();
        send(&mut out, &["AUTH", "user", "pw"]).await.unwrap();
        assert_eq!(out, b"*3\r\n$4\r\nAUTH\r\n$4\r\nuser\r\n$2\r\npw\r\n");
    }
}
