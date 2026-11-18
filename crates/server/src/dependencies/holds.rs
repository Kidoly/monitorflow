//! What is held now, and why (ADR-0152).
//!
//! From the dependencies and a few facts about their parents (which are
//! down, which may be about to be, which recovered lately), [`Holds::new`]
//! decides for every child:
//!
//! - **unreachable** while a parent is down, or is itself unreachable: it
//!   opens no incident and the nearest parent that is down speaks for it;
//! - **in doubt** while a parent may be about to go down, an agent or relay
//!   without a stream or a check whose critical condition has not lasted
//!   its duration yet: it waits for the parent's own decision;
//! - **settling** for [`SETTLE`] after a parent's incident resolved.
//!
//! A check is held when its server is held, not when its server is down:
//! the checks of a device whose ping fails still say what they see, unless
//! they are set to depend on that ping.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{CheckRef, Edge, Node, SETTLE};
use crate::alerting::offline::OFFLINE_METRIC;
use crate::notify::silence::check_covers;

/// A parent that is down, with the incident that says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Down {
    pub incident: Uuid,
    pub since: DateTime<Utc>,
}

/// What [`super::store`] knows of the parents.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub down: HashMap<Node, Down>,
    pub doubt: HashSet<Node>,
    /// The last time a parent's down incident resolved, within
    /// [`super::RECENT`].
    pub recovered: HashMap<Node, DateTime<Utc>>,
    /// Every check name the central server knows, to tell which check
    /// wrote a metric.
    pub names: Arc<Vec<String>>,
}

/// Why a child is held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hold {
    /// `cause` is the nearest parent that is down, `incident` its incident.
    Unreachable {
        cause: Node,
        incident: Uuid,
    },
    Doubt,
    /// Judged again from `until`.
    Settling {
        until: DateTime<Utc>,
    },
}

impl Hold {
    fn rank(&self) -> u8 {
        match self {
            Self::Unreachable { .. } => 3,
            Self::Doubt => 2,
            Self::Settling { .. } => 1,
        }
    }

    /// The stronger of two holds; of two settlings, the later.
    fn max(self, other: Self) -> Self {
        match (&self, &other) {
            (Self::Settling { until: a }, Self::Settling { until: b }) => {
                if b > a {
                    other
                } else {
                    self
                }
            }
            _ if other.rank() > self.rank() => other,
            _ => self,
        }
    }

    pub fn unreachable(&self) -> Option<(&Node, Uuid)> {
        match self {
            Self::Unreachable { cause, incident } => Some((cause, *incident)),
            _ => None,
        }
    }
}

/// The holds of every child now, from [`Holds::new`].
#[derive(Debug, Clone, Default)]
pub struct Holds {
    held: HashMap<Node, Hold>,
    /// For each child, the last recovery of anything it is behind.
    heard: HashMap<Node, DateTime<Utc>>,
    /// Held checks by server, for series.
    checks: HashMap<Uuid, Vec<(CheckRef, Hold)>>,
    /// Children by parent, for what a parent hides.
    children: HashMap<Node, Vec<Node>>,
    down: HashMap<Node, Down>,
    names: Arc<Vec<String>>,
}

impl Holds {
    pub fn new(edges: &[Edge], facts: Facts, now: DateTime<Utc>) -> Self {
        let mut by_child: HashMap<&Node, Vec<&Node>> = HashMap::new();
        let mut children: HashMap<Node, Vec<Node>> = HashMap::new();
        for edge in edges {
            by_child.entry(&edge.child).or_default().push(&edge.parent);
            let list = children.entry(edge.parent.clone()).or_default();
            if !list.contains(&edge.child) {
                list.push(edge.child.clone());
            }
        }
        // Every node that may be held: children, and the checks that are
        // parents, which are behind their own server.
        let mut nodes: Vec<Node> = by_child.keys().map(|n| (*n).clone()).collect();
        nodes.extend(
            edges
                .iter()
                .filter(|e| matches!(e.parent, Node::Check(..)))
                .map(|e| e.parent.clone()),
        );
        nodes.sort();
        nodes.dedup();
        let mut walk = Walk {
            by_child: &by_child,
            facts: &facts,
            now,
            held: HashMap::new(),
            heard: HashMap::new(),
            busy: HashSet::new(),
        };
        for node in &nodes {
            walk.hold(node);
        }
        let Walk { held, heard, .. } = walk;
        let held: HashMap<Node, Hold> = held
            .into_iter()
            .filter_map(|(node, hold)| Some((node, hold?)))
            .collect();
        let heard = heard
            .into_iter()
            .filter_map(|(node, at)| Some((node, at?)))
            .collect();
        let mut checks: HashMap<Uuid, Vec<(CheckRef, Hold)>> = HashMap::new();
        for (node, hold) in &held {
            if let (Some(server), Some(check)) = (node.server(), node.check_ref()) {
                checks
                    .entry(server)
                    .or_default()
                    .push((check, hold.clone()));
            }
        }
        Self {
            held,
            heard,
            checks,
            children,
            down: facts.down,
            names: facts.names,
        }
    }

    /// How `node` is held, if it is.
    pub fn of(&self, node: &Node) -> Option<&Hold> {
        self.held.get(node)
    }

    /// The parent that is down with its incident, when `node` is.
    pub fn down(&self, node: &Node) -> Option<&Down> {
        self.down.get(node)
    }

    /// How the series `metric` of `instance` of `server` is held: by its
    /// server, or by a held check that wrote it. The offline incident is
    /// about the server only.
    pub fn series(&self, server: Uuid, metric: &str, instance: &str) -> Option<Hold> {
        let whole = self.held.get(&Node::Server(server)).cloned();
        let checks = (metric != OFFLINE_METRIC)
            .then(|| self.checks.get(&server))
            .flatten()
            .into_iter()
            .flatten()
            .filter(|(check, _)| check_covers(check, &self.names, metric, instance))
            .map(|(_, hold)| hold.clone());
        whole.into_iter().chain(checks).reduce(Hold::max)
    }

    /// The last recovery of anything `node` is behind, within
    /// [`super::RECENT`]: the offline rule counts it as heard from.
    pub fn heard(&self, node: &Node) -> Option<DateTime<Utc>> {
        self.heard.get(node).copied()
    }

    /// Everything behind `node`, nearest first, each once. A server's
    /// checks that others depend on are behind it too.
    pub fn behind(&self, node: &Node) -> Vec<Node> {
        let mut seen: HashSet<Node> = HashSet::from([node.clone()]);
        let mut queue = VecDeque::from([node.clone()]);
        let mut found = Vec::new();
        while let Some(at) = queue.pop_front() {
            let own_checks = match &at {
                Node::Server(id) => self
                    .children
                    .keys()
                    .filter(|n| matches!(n, Node::Check(s, ..) if s == id))
                    .cloned()
                    .collect(),
                _ => Vec::new(),
            };
            let direct = self.children.get(&at).cloned().unwrap_or_default();
            for child in direct {
                if seen.insert(child.clone()) {
                    found.push(child.clone());
                    queue.push_back(child);
                }
            }
            // Not listed themselves: their children are.
            for check in own_checks {
                if seen.insert(check.clone()) {
                    queue.push_back(check);
                }
            }
        }
        found
    }
}

struct Walk<'a> {
    by_child: &'a HashMap<&'a Node, Vec<&'a Node>>,
    facts: &'a Facts,
    now: DateTime<Utc>,
    held: HashMap<Node, Option<Hold>>,
    heard: HashMap<Node, Option<DateTime<Utc>>>,
    /// Nodes being decided, so a loop that got in anyway ends.
    busy: HashSet<Node>,
}

impl Walk<'_> {
    fn hold(&mut self, node: &Node) -> Option<Hold> {
        if let Some(done) = self.held.get(node) {
            return done.clone();
        }
        if !self.busy.insert(node.clone()) {
            return None;
        }
        let mut best: Option<Hold> = None;
        let mut heard: Option<DateTime<Utc>> = None;
        let parents: Vec<Node> = self
            .by_child
            .get(node)
            .into_iter()
            .flatten()
            .map(|p| (*p).clone())
            .collect();
        for parent in &parents {
            let recovered = self.facts.recovered.get(parent).copied();
            let candidate = if let Some(down) = self.facts.down.get(parent) {
                Some(Hold::Unreachable {
                    cause: parent.clone(),
                    incident: down.incident,
                })
            } else if self.facts.doubt.contains(parent) {
                Some(Hold::Doubt)
            } else {
                let settling = recovered
                    .map(|at| at + SETTLE)
                    .filter(|until| *until > self.now)
                    .map(|until| Hold::Settling { until });
                [self.hold(parent), settling]
                    .into_iter()
                    .flatten()
                    .reduce(Hold::max)
            };
            best = [best, candidate].into_iter().flatten().reduce(Hold::max);
            let above = self.heard_of(parent);
            heard = [heard, recovered, above].into_iter().flatten().max();
        }
        // A check is held when its server is.
        if let Node::Check(server, ..) = node {
            let whole = Node::Server(*server);
            best = [best, self.hold(&whole)]
                .into_iter()
                .flatten()
                .reduce(Hold::max);
            heard = [heard, self.heard_of(&whole)].into_iter().flatten().max();
        }
        self.busy.remove(node);
        self.held.insert(node.clone(), best.clone());
        self.heard.insert(node.clone(), heard);
        best
    }

    fn heard_of(&mut self, node: &Node) -> Option<DateTime<Utc>> {
        self.hold(node);
        self.heard.get(node).copied().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minute: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + minute * 60, 0).unwrap()
    }

    fn server(n: u128) -> Node {
        Node::Server(Uuid::from_u128(n))
    }

    fn check(n: u128, name: &str) -> Node {
        Node::Check(Uuid::from_u128(n), name.into(), String::new())
    }

    fn edge(child: Node, parent: Node) -> Edge {
        Edge {
            id: Some(Uuid::new_v4()),
            child,
            parent,
        }
    }

    fn down(n: u128) -> Down {
        Down {
            incident: Uuid::from_u128(1000 + n),
            since: at(0),
        }
    }

    fn names() -> Arc<Vec<String>> {
        Arc::new(
            ["ping", "https", "service_nginx"]
                .map(String::from)
                .to_vec(),
        )
    }

    #[test]
    fn a_tree_is_unreachable_behind_its_nearest_parent_down() {
        // router (1) <- switch (2) <- web-1 (3) and web-2 (4).
        let edges = [
            edge(server(2), server(1)),
            edge(server(3), server(2)),
            edge(server(4), server(2)),
        ];
        let facts = Facts {
            down: HashMap::from([(server(1), down(1))]),
            ..Facts::default()
        };
        let holds = Holds::new(&edges, facts, at(1));
        for n in [2, 3, 4] {
            assert_eq!(
                holds.of(&server(n)),
                Some(&Hold::Unreachable {
                    cause: server(1),
                    incident: down(1).incident
                }),
                "{n}"
            );
        }
        assert_eq!(holds.of(&server(1)), None);
        assert_eq!(holds.behind(&server(1)), [server(2), server(3), server(4)]);
        // The switch down too: its own incident speaks for what is behind it.
        let facts = Facts {
            down: HashMap::from([(server(1), down(1)), (server(2), down(2))]),
            ..Facts::default()
        };
        let holds = Holds::new(&edges, facts, at(1));
        assert_eq!(
            holds.of(&server(3)).unwrap().unreachable().unwrap().0,
            &server(2)
        );
        assert_eq!(
            holds.of(&server(2)).unwrap().unreachable().unwrap().0,
            &server(1)
        );
    }

    #[test]
    fn any_parent_down_is_enough_and_doubt_waits() {
        let edges = [edge(server(3), server(1)), edge(server(3), server(2))];
        let facts = Facts {
            down: HashMap::from([(server(2), down(2))]),
            doubt: HashSet::from([server(1)]),
            ..Facts::default()
        };
        let holds = Holds::new(&edges, facts, at(1));
        assert!(holds.of(&server(3)).unwrap().unreachable().is_some());
        let facts = Facts {
            doubt: HashSet::from([server(1)]),
            ..Facts::default()
        };
        assert_eq!(
            Holds::new(&edges, facts, at(1)).of(&server(3)),
            Some(&Hold::Doubt)
        );
        assert_eq!(
            Holds::new(&edges, Facts::default(), at(1)).of(&server(3)),
            None
        );
    }

    #[test]
    fn children_settle_for_two_minutes_after_the_parent_recovers() {
        let edges = [edge(server(2), server(1)), edge(server(3), server(2))];
        let facts = || Facts {
            recovered: HashMap::from([(server(1), at(10))]),
            ..Facts::default()
        };
        let holds = Holds::new(&edges, facts(), at(11));
        assert_eq!(
            holds.of(&server(2)),
            Some(&Hold::Settling { until: at(12) })
        );
        // Behind the switch too, which was unreachable with them.
        assert_eq!(
            holds.of(&server(3)),
            Some(&Hold::Settling { until: at(12) })
        );
        assert_eq!(holds.heard(&server(3)), Some(at(10)));
        let holds = Holds::new(&edges, facts(), at(12));
        assert_eq!(holds.of(&server(3)), None);
        // Still heard from, for the offline rule's own five minutes.
        assert_eq!(holds.heard(&server(3)), Some(at(10)));
    }

    #[test]
    fn checks_follow_their_server_and_their_own_parents() {
        let device = 2;
        // The device's https behind its own ping; web-1 (3) behind the
        // uplink check of the switch (1).
        let edges = [
            edge(check(device, "https"), check(device, "ping")),
            edge(server(3), check(1, "uplink")),
        ];
        let facts = Facts {
            down: HashMap::from([(check(device, "ping"), down(2))]),
            names: names(),
            ..Facts::default()
        };
        let holds = Holds::new(&edges, facts, at(1));
        let id = Uuid::from_u128(device);
        assert!(holds
            .series(id, "monitorflow_https_up", "")
            .unwrap()
            .unreachable()
            .is_some());
        // The ping check itself, and other checks, are not held.
        assert_eq!(holds.series(id, "monitorflow_ping_up", ""), None);
        assert_eq!(
            holds.series(id, "monitorflow_service_nginx_active", ""),
            None
        );
        assert_eq!(holds.series(Uuid::from_u128(3), "offline", ""), None);

        // The switch held by the router: its uplink check is too, and what
        // depends on that check.
        let edges = [
            edge(server(1), server(9)),
            edge(server(3), check(1, "uplink")),
        ];
        let facts = Facts {
            down: HashMap::from([(server(9), down(9))]),
            names: names(),
            ..Facts::default()
        };
        let holds = Holds::new(&edges, facts, at(1));
        assert!(holds.of(&check(1, "uplink")).is_some());
        let web = holds.series(Uuid::from_u128(3), "offline", "").unwrap();
        assert_eq!(web.unreachable().unwrap().0, &server(9));
        assert_eq!(holds.behind(&server(9)), [server(1), server(3)]);
    }

    #[test]
    fn a_server_down_does_not_hold_its_own_checks() {
        let edges = [edge(server(3), server(2))];
        let facts = Facts {
            down: HashMap::from([(server(2), down(2))]),
            names: names(),
            ..Facts::default()
        };
        let holds = Holds::new(&edges, facts, at(1));
        assert_eq!(
            holds.series(Uuid::from_u128(2), "monitorflow_https_up", ""),
            None
        );
        assert!(holds
            .series(Uuid::from_u128(3), "monitorflow_https_up", "")
            .is_some());
    }

    #[test]
    fn a_loop_that_got_in_ends() {
        let edges = [edge(server(1), server(2)), edge(server(2), server(1))];
        let holds = Holds::new(&edges, Facts::default(), at(1));
        assert_eq!(holds.of(&server(1)), None);
        assert_eq!(holds.behind(&server(1)), [server(2)]);
    }
}
