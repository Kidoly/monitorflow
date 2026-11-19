//! Dependencies (ADR-0152): a server, a device or one of its checks is
//! behind a parent, a server, a device, a relay or a check. While a parent
//! is down, what is behind it opens no incident and notifies nobody; it is
//! unreachable, and the parent's incident speaks for it.
//!
//! [`graph`] refuses cycles, [`holds`] decides from the facts what is held
//! and why, and [`store`] reads the dependencies and those facts from
//! PostgreSQL. The threshold engine, the offline rule and the dispatcher
//! each read a [`Holds`] and leave the rest of their rules as they were.

pub mod graph;
pub mod holds;
pub mod store;

use std::fmt;

use uuid::Uuid;

pub use crate::notify::silence::CheckRef;
pub use holds::{Facts, Hold, Holds};

/// How long after a parent's incident resolves its children are judged
/// again: two runs of a probe check at its default minute, and the time a
/// switch's links take to settle.
pub const SETTLE: chrono::TimeDelta = chrono::TimeDelta::minutes(2);

/// How far back a resolved incident still counts as a recovery, for the
/// offline rule, whose servers get 5 minutes after it to reconnect.
pub const RECENT: chrono::TimeDelta = chrono::TimeDelta::minutes(10);

/// One end of a dependency.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Node {
    /// A server or a device, as a whole.
    Server(Uuid),
    /// One check of a server or device; an instance of `""` is every
    /// instance of it.
    Check(Uuid, String, String),
    Relay(Uuid),
}

impl Node {
    pub fn check(server: Uuid, check: &CheckRef) -> Self {
        Self::Check(server, check.name.clone(), check.instance.clone())
    }

    /// The server or device the node is or belongs to.
    pub fn server(&self) -> Option<Uuid> {
        match self {
            Self::Server(id) | Self::Check(id, ..) => Some(*id),
            Self::Relay(_) => None,
        }
    }

    pub fn check_ref(&self) -> Option<CheckRef> {
        match self {
            Self::Check(_, name, instance) => Some(CheckRef {
                name: name.clone(),
                instance: instance.clone(),
            }),
            _ => None,
        }
    }
}

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Server(id) => write!(f, "server {id}"),
            Self::Check(id, name, instance) if instance.is_empty() => {
                write!(f, "check {name} of {id}")
            }
            Self::Check(id, name, instance) => write!(f, "check {name} ({instance}) of {id}"),
            Self::Relay(id) => write!(f, "relay {id}"),
        }
    }
}

/// A child behind a parent. `id` is the row of `dependencies`, `None` for
/// the automatic ones: a server behind its relay, a check behind the relay
/// that runs it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Edge {
    pub id: Option<Uuid>,
    pub child: Node,
    pub parent: Node,
}

impl Edge {
    pub fn automatic(child: Node, parent: Node) -> Self {
        Self {
            id: None,
            child,
            parent,
        }
    }
}
