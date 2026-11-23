//! `redis`: what `INFO` says: clients, memory against `maxmemory`,
//! evictions, keyspace hit ratio, persistence and replication.
//!
//! Parameters: `host` (`127.0.0.1`), `port` (6379), `user` (an ACL user;
//! the default user without one), `password`. The account needs `INFO`
//! and `PING` only. Encryption is not offered yet: `tls` accepts `off`.

use std::collections::HashMap;
use std::time::Instant;

use monitorflow_sensors::{Sample, Sensor, SensorError};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::rates::{Rates, Round};
use crate::resp::{self, Reply, RespError};
use crate::{down, gauge, label, ratio, within, Failure, Params, Place, Reason, Target, Tls};

/// Replicas reported by name.
const MAX_REPLICAS: usize = 20;

#[derive(Debug)]
pub struct Redis {
    target: Target,
    rates: Rates,
}

impl Redis {
    pub(crate) fn from_params(
        p: &mut Params<'_>,
        place: Place<'_>,
    ) -> Result<Self, monitorflow_sensors::BuildError> {
        let target = Target::parse(p, place, 6379, None, Tls::Off)?;
        if target.tls != Tls::Off {
            return Err(p.bad("tls", "must be `off`: encrypted Redis is not checked yet"));
        }
        Ok(Self {
            target,
            rates: Rates::default(),
        })
    }

    async fn read(&self, round: &mut Round) -> Result<Vec<Sample>, Failure> {
        let started = Instant::now();
        let t = &self.target;
        let stream = TcpStream::connect((t.host.as_str(), t.port))
            .await
            .map_err(|e| Failure::new(Reason::Connect, e.to_string()))?;
        let mut stream = BufReader::new(stream);
        let io = |e: RespError| match e {
            RespError::Io(e) => Failure::new(Reason::Connect, e.to_string()),
            RespError::Protocol(e) => Failure::new(Reason::Protocol, e),
        };
        if let Some(password) = &t.password {
            let mut args = vec!["AUTH"];
            args.extend(t.user.as_deref());
            args.push(password);
            resp::send(&mut stream, &args).await.map_err(io)?;
            match resp::read(&mut stream).await.map_err(io)? {
                Reply::Simple(_) => {}
                Reply::Error(e) => return Err(Failure::new(Reason::Login, e)),
                _ => return Err(Failure::new(Reason::Protocol, "an unexpected reply to AUTH")),
            }
        }
        resp::send(&mut stream, &["INFO"]).await.map_err(io)?;
        let info = match resp::read(&mut stream).await.map_err(io)? {
            Reply::Bulk(Some(info)) => info,
            Reply::Error(e) if e.starts_with("NOAUTH") || e.starts_with("WRONGPASS") => {
                return Err(Failure::new(Reason::Login, e))
            }
            Reply::Error(e) if e.starts_with("NOPERM") => {
                return Err(Failure::new(Reason::Permission, e))
            }
            Reply::Error(e) => return Err(Failure::new(Reason::Protocol, e)),
            _ => return Err(Failure::new(Reason::Protocol, "an unexpected reply to INFO")),
        };
        let response = started.elapsed().as_secs_f64();
        let _ = stream.get_mut().shutdown().await;
        Ok(samples(&parse_info(&info), response, round))
    }
}

/// `INFO`'s `key:value` lines.
pub fn parse_info(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect()
}

/// The `a=1,b=2` fields of a keyspace or replica line.
fn fields(value: &str) -> HashMap<&str, &str> {
    value.split(',').filter_map(|f| f.split_once('=')).collect()
}

/// The samples of one `INFO`.
pub(crate) fn samples(info: &HashMap<String, String>, response: f64, round: &mut Round) -> Vec<Sample> {
    let num = |key: &str| info.get(key).and_then(|v| v.parse::<f64>().ok());
    let role = info.get("role").map_or("", String::as_str);
    let mut out = vec![
        Sample::metric("up", 1.0)
            .with_label(
                "version",
                label(info.get("redis_version").map_or("", String::as_str), 64),
            )
            .with_label("role", if role == "slave" { "replica" } else { "primary" }),
        Sample::metric("response_seconds", response),
    ];
    out.extend(num("connected_clients").and_then(|v| gauge("clients", v)));
    out.extend(num("blocked_clients").and_then(|v| gauge("blocked_clients", v)));
    out.extend(
        num("rejected_connections")
            .and_then(|v| round.delta("rejected", v))
            .and_then(|v| gauge("rejected_connections", v)),
    );

    let used = num("used_memory");
    let max = num("maxmemory").filter(|m| *m > 0.0);
    out.extend(used.and_then(|v| gauge("memory_used_bytes", v)));
    out.extend(max.and_then(|v| gauge("memory_max_bytes", v)));
    if let (Some(used), Some(max)) = (used, max) {
        out.extend(ratio(used, max).map(|r| Sample::metric("memory_usage_ratio", r)));
    }
    out.extend(
        num("mem_fragmentation_ratio").and_then(|v| gauge("memory_fragmentation_ratio", v)),
    );
    out.extend(
        num("evicted_keys")
            .and_then(|v| round.delta("evicted", v))
            .and_then(|v| gauge("evicted_keys", v)),
    );
    if let (Some(hits), Some(misses)) = (num("keyspace_hits"), num("keyspace_misses")) {
        out.extend(
            round
                .hit_ratio("keyspace", hits, misses)
                .and_then(|r| gauge("keyspace_hit_ratio", r)),
        );
    }
    let keys: f64 = info
        .iter()
        .filter(|(k, _)| k.strip_prefix("db").is_some_and(|n| n.parse::<u32>().is_ok()))
        .filter_map(|(_, v)| fields(v).get("keys").and_then(|k| k.parse::<f64>().ok()))
        .sum();
    out.push(Sample::metric("keys", keys));

    if let Some(status) = info.get("rdb_last_bgsave_status") {
        out.push(Sample::metric("last_save_ok", f64::from(u8::from(status == "ok"))));
    }
    if info.get("aof_enabled").map(String::as_str) == Some("1") {
        if let Some(status) = info.get("aof_last_write_status") {
            out.push(Sample::metric("aof_write_ok", f64::from(u8::from(status == "ok"))));
        }
    }

    if role == "slave" {
        let up = info.get("master_link_status").map(String::as_str) == Some("up");
        out.push(Sample::metric("primary_link_up", f64::from(u8::from(up))));
        out.extend(
            num("master_last_io_seconds_ago")
                .filter(|s| *s >= 0.0)
                .and_then(|v| gauge("primary_last_io_seconds", v)),
        );
    } else {
        out.extend(num("connected_slaves").and_then(|v| gauge("replicas", v)));
        let offset = num("master_repl_offset");
        let mut replicas: Vec<(&String, &String)> = info
            .iter()
            .filter(|(k, _)| k.strip_prefix("slave").is_some_and(|n| n.parse::<u32>().is_ok()))
            .collect();
        replicas.sort();
        for (_, value) in replicas.into_iter().take(MAX_REPLICAS) {
            let f = fields(value);
            let name = format!(
                "{}:{}",
                f.get("ip").copied().unwrap_or(""),
                f.get("port").copied().unwrap_or("")
            );
            if let (Some(offset), Some(theirs)) =
                (offset, f.get("offset").and_then(|o| o.parse::<f64>().ok()))
            {
                out.push(
                    Sample::metric("replica_lag_bytes", (offset - theirs).max(0.0))
                        .with_instance(label(&name, 128)),
                );
            }
        }
    }
    out
}

#[async_trait::async_trait]
impl Sensor for Redis {
    async fn collect(&self, deadline: tokio::time::Instant) -> Result<Vec<Sample>, SensorError> {
        let mut round = self.rates.round();
        match within(deadline, self.read(&mut round)).await {
            Ok(samples) => {
                self.rates.finish(round);
                Ok(samples)
            }
            Err(failure) => {
                self.rates.reset();
                Ok(vec![down(Some(&self.target), &failure)])
            }
        }
    }
}
