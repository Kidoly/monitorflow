//! The path of a call through chained relays (ADR-0028).
//!
//! A relay that forwards a stream or a renewal upstream adds the
//! `monitorflow-via` metadata entry: peer URIs, comma-separated, from the
//! origin of the call to the peer the relay received it from. Each relay
//! appends only the peer whose certificate it verified itself, so a relay
//! can lie about what is below it but never about who sent it the call.
//! The receiver of the call adds its own verified view of the sender: the
//! connecting peer is never in the list.
//!
//! The server checks the list against the relays' parents in its database,
//! which bounds what a compromised relay can claim to its own subtree.

// Every check here ends as the `Status` tonic returns by value.
#![allow(clippy::result_large_err)]

use std::fmt;

use tonic::metadata::{MetadataMap, MetadataValue};
use tonic::Status;

use crate::tls::{PeerIdentity, PeerRole};

/// Metadata key of the path.
pub const HEADER: &str = "monitorflow-via";
/// Most peers in a path, origin included: chains deeper than this are
/// refused, so a loop or a hostile relay cannot grow the header forever.
pub const MAX_HOPS: usize = 8;

/// The peers a call went through below the connecting peer: the origin
/// first, then each relay towards the top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Via {
    peers: Vec<PeerIdentity>,
}

impl Via {
    /// A path that starts with `origin`, which sent the call itself.
    pub fn origin(origin: PeerIdentity) -> Self {
        Self {
            peers: vec![origin],
        }
    }

    /// The peer the call is about: an agent, or a relay speaking for
    /// itself.
    pub fn origin_peer(&self) -> &PeerIdentity {
        &self.peers[0]
    }

    /// Relays between the origin and the connecting peer, from the one the
    /// origin is attached to upwards.
    pub fn relays(&self) -> impl DoubleEndedIterator<Item = &PeerIdentity> {
        self.peers[1..].iter()
    }

    /// The path a relay sends upstream for a call it received from `sender`
    /// with `received` (the sender's own path, if any). An agent always
    /// speaks for itself, so what it sent is ignored; a relay without a
    /// path speaks for itself too.
    pub fn extend(received: Option<Via>, sender: &PeerIdentity) -> Result<Self, Status> {
        match (sender.role(), received) {
            (PeerRole::Relay, Some(mut via)) => {
                if via.peers.len() >= MAX_HOPS {
                    return Err(Status::permission_denied(format!(
                        "relay chain longer than {MAX_HOPS} peers"
                    )));
                }
                via.peers.push(sender.clone());
                Ok(via)
            }
            (PeerRole::Server, _) => Err(Status::permission_denied(
                "role server may not send calls through a relay",
            )),
            _ => Ok(Self::origin(sender.clone())),
        }
    }

    /// Reads the path of `metadata`; `None` without one.
    pub fn from_metadata(metadata: &MetadataMap) -> Result<Option<Self>, Status> {
        let Some(value) = metadata.get(HEADER) else {
            return Ok(None);
        };
        let bad = |why: String| Status::invalid_argument(format!("invalid {HEADER}: {why}"));
        let text = value.to_str().map_err(|e| bad(e.to_string()))?;
        let peers = text
            .split(',')
            .map(|uri| PeerIdentity::from_uri(uri.trim()).map_err(|e| bad(e.to_string())))
            .collect::<Result<Vec<_>, _>>()?;
        if peers.len() > MAX_HOPS {
            return Err(bad(format!("more than {MAX_HOPS} peers")));
        }
        if peers[1..].iter().any(|p| p.role() != PeerRole::Relay)
            || peers[0].role() == PeerRole::Server
        {
            return Err(bad(
                "only the origin may be an agent, and no peer a server".into()
            ));
        }
        Ok(Some(Self { peers }))
    }

    /// Writes the path into `metadata`, replacing any.
    pub fn insert_into(&self, metadata: &mut MetadataMap) {
        let value = MetadataValue::try_from(self.to_string())
            .expect("peer URIs are visible ASCII without commas");
        metadata.insert(HEADER, value);
    }
}

impl fmt::Display for Via {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (n, peer) in self.peers.iter().enumerate() {
            if n > 0 {
                f.write_str(",")?;
            }
            f.write_str(&peer.uri())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tonic::Code;

    use super::*;

    fn peer(role: PeerRole, id: &str) -> PeerIdentity {
        PeerIdentity::new(role, id).unwrap()
    }

    #[test]
    fn each_relay_appends_the_peer_it_verified() {
        let agent = peer(PeerRole::Agent, "web-01");
        let b = peer(PeerRole::Relay, "b");
        // Relay b got the call from the agent; whatever the agent sent is
        // ignored.
        let forged = Via::origin(peer(PeerRole::Agent, "web-99"));
        let at_b = Via::extend(Some(forged), &agent).unwrap();
        assert_eq!(at_b.to_string(), "monitorflow://agent/web-01");
        // Relay a got it from b.
        let at_a = Via::extend(Some(at_b), &b).unwrap();
        assert_eq!(
            at_a.to_string(),
            "monitorflow://agent/web-01,monitorflow://relay/b"
        );
        assert_eq!(at_a.origin_peer(), &agent);
        assert_eq!(at_a.relays().collect::<Vec<_>>(), vec![&b]);
        // A relay without a path speaks for itself.
        assert_eq!(Via::extend(None, &b).unwrap(), Via::origin(b.clone()));
        let server = peer(PeerRole::Server, "central");
        assert!(Via::extend(None, &server).is_err());
    }

    #[test]
    fn metadata_round_trips_and_bad_paths_are_refused() {
        let mut via = Via::origin(peer(PeerRole::Agent, "web-01"));
        via = Via::extend(Some(via), &peer(PeerRole::Relay, "b")).unwrap();
        let mut metadata = MetadataMap::new();
        assert_eq!(Via::from_metadata(&metadata).unwrap(), None);
        via.insert_into(&mut metadata);
        assert_eq!(Via::from_metadata(&metadata).unwrap(), Some(via.clone()));

        for bad in [
            "",
            "monitorflow://agent/a,monitorflow://agent/b",
            "monitorflow://server/central",
            "monitorflow://relay/b,monitorflow://server/central",
            "https://example",
        ] {
            let mut metadata = MetadataMap::new();
            metadata.insert(HEADER, bad.parse().unwrap());
            let status = Via::from_metadata(&metadata).unwrap_err();
            assert_eq!(status.code(), Code::InvalidArgument, "{bad}");
        }

        let mut long = Via::origin(peer(PeerRole::Agent, "web-01"));
        for n in 1..MAX_HOPS {
            long = Via::extend(Some(long), &peer(PeerRole::Relay, &format!("r{n}"))).unwrap();
        }
        let status = Via::extend(Some(long), &peer(PeerRole::Relay, "top")).unwrap_err();
        assert_eq!(status.code(), Code::PermissionDenied);
    }
}
