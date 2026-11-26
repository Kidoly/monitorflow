//! The connection of a remote desktop (ADR-0190): the host's own RDP
//! service on [`RDP_ADDRESS`], and nothing else.
//!
//! No address comes from the central server: the command names a kind,
//! not a place, so a central server that is taken over cannot use the
//! agent to reach another machine or another port. The agent carries
//! bytes and reads none of them; the central server speaks RDP, TLS
//! included, with the service inside the sealed frames, and the person
//! signs in to Windows as usual.

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::remote::MAX_CHUNK;
use tokio::sync::mpsc;

use super::{Control, Spawner, Terminal};

/// Where Windows' Remote Desktop service listens. Loopback reaches it
/// without any firewall rule.
pub const RDP_ADDRESS: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 3389);
/// How long the service has to accept the connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Chunks read from the service waiting for the session.
const OUTPUT_QUEUE: usize = 16;

/// The platform's desktop connection: [`RDP_ADDRESS`].
pub fn spawner() -> Spawner {
    connector(RDP_ADDRESS)
}

/// A connection to `address`, which must be a loopback address: tests
/// give a fake service's port, the agent only [`RDP_ADDRESS`].
pub fn connector(address: SocketAddr) -> Spawner {
    Arc::new(move |_spec| connect(address))
}

fn connect(address: SocketAddr) -> std::io::Result<Terminal> {
    if !address.ip().is_loopback() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "a remote desktop reaches this host's own service only",
        ));
    }
    let stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)?;
    stream.set_nodelay(true)?;
    let mut reader = stream.try_clone()?;
    let mut writer = stream.try_clone()?;
    let (output_tx, output) = mpsc::channel(OUTPUT_QUEUE);
    std::thread::Builder::new()
        .name("rdp-read".into())
        .spawn(move || {
            let mut buffer = vec![0u8; MAX_CHUNK];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    // Blocks while the session's window is full, so the
                    // service waits too.
                    Ok(n) => {
                        if output_tx.blocking_send(buffer[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        })?;
    let (input, input_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::Builder::new()
        .name("rdp-write".into())
        .spawn(move || {
            for chunk in input_rx {
                if writer.write_all(&chunk).is_err() {
                    break;
                }
            }
            let _ = writer.shutdown(Shutdown::Write);
        })?;
    Ok(Terminal {
        output,
        input,
        control: Box::new(Connection(stream)),
    })
}

/// Ends the connection; a desktop has no size of its own to change.
struct Connection(TcpStream);

impl Control for Connection {
    fn resize(&self, _cols: u16, _rows: u16) -> std::io::Result<()> {
        Ok(())
    }

    fn kill(&mut self) {
        let _ = self.0.shutdown(Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::Spec;

    fn spec() -> Spec {
        Spec {
            session_id: "0b5c8a52-6c43-4a6e-8f1e-9d8a3f1c2b7e".into(),
            requested_by: "alice".into(),
            cols: 0,
            rows: 0,
            request: Vec::new(),
        }
    }

    #[test]
    fn reaches_only_the_hosts_own_rdp_service() {
        assert_eq!(RDP_ADDRESS.to_string(), "127.0.0.1:3389");
        let elsewhere: SocketAddr = "192.0.2.10:3389".parse().unwrap();
        let error = connector(elsewhere)(&spec()).err().unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[tokio::test]
    async fn carries_bytes_both_ways_and_ends_with_the_service() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let service = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut hello = [0u8; 5];
            socket.read_exact(&mut hello).unwrap();
            socket.write_all(b"olleh").unwrap();
            hello
        });
        let spawn = connector(address);
        let mut terminal = tokio::task::spawn_blocking(move || spawn(&spec()))
            .await
            .unwrap()
            .unwrap();
        terminal.input.send(b"hello".to_vec()).unwrap();
        assert_eq!(terminal.output.recv().await.unwrap(), b"olleh");
        assert_eq!(&service.join().unwrap(), b"hello");
        // The service closed its side.
        assert_eq!(terminal.output.recv().await, None);
        terminal.control.kill();
    }
}
