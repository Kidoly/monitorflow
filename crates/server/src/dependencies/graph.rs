//! Cycles (ADR-0152): a dependency that would put something behind itself
//! is refused, with the path that would close the loop.
//!
//! "Behind" follows every dependency, the automatic ones included, and one
//! more step: a check is behind its own server. So a server cannot depend
//! on one of its own checks, while one check of a server may depend on
//! another of its checks.

use std::collections::{HashMap, HashSet, VecDeque};

use super::{Edge, Node};

/// What `node` is directly behind, among `edges` and `adding`.
fn parents<'a>(
    by_child: &'a HashMap<&'a Node, Vec<&'a Node>>,
    node: &'a Node,
) -> impl Iterator<Item = Node> + 'a {
    let own = match node {
        Node::Check(server, ..) => Some(Node::Server(*server)),
        _ => None,
    };
    by_child
        .get(node)
        .into_iter()
        .flatten()
        .map(|parent| (*parent).clone())
        .chain(own)
}

/// The first of `adding` (child, parent) that would close a loop with
/// `edges` and the others of `adding`, as the path from its parent up to
/// its child: `[parent, ..., child]`. `None` when there is none.
pub fn cycle(edges: &[Edge], adding: &[(Node, Node)]) -> Option<(Node, Vec<Node>)> {
    let mut by_child: HashMap<&Node, Vec<&Node>> = HashMap::new();
    for edge in edges {
        by_child.entry(&edge.child).or_default().push(&edge.parent);
    }
    for (child, parent) in adding {
        by_child.entry(child).or_default().push(parent);
    }
    for (child, parent) in adding {
        if let Some(path) = path(&by_child, parent, child) {
            return Some((child.clone(), path));
        }
    }
    None
}

/// The shortest way up from `from` to `to`, both included.
fn path(by_child: &HashMap<&Node, Vec<&Node>>, from: &Node, to: &Node) -> Option<Vec<Node>> {
    let mut came_from: HashMap<Node, Node> = HashMap::new();
    let mut seen: HashSet<Node> = HashSet::from([from.clone()]);
    let mut queue = VecDeque::from([from.clone()]);
    while let Some(node) = queue.pop_front() {
        if &node == to {
            let mut path = vec![node.clone()];
            let mut at = node;
            while let Some(previous) = came_from.get(&at) {
                path.push(previous.clone());
                at = previous.clone();
            }
            path.reverse();
            return Some(path);
        }
        let next: Vec<Node> = parents(by_child, &node).collect();
        for parent in next {
            if seen.insert(parent.clone()) {
                came_from.insert(parent.clone(), node.clone());
                queue.push_back(parent);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

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

    #[test]
    fn a_chain_is_fine_and_its_loop_is_refused_with_the_path() {
        // web-1 (1) behind fw-1 (2) behind sw-core (3).
        let edges = [edge(server(1), server(2)), edge(server(2), server(3))];
        assert_eq!(cycle(&edges, &[(server(4), server(1))]), None);
        let (child, path) = cycle(&edges, &[(server(3), server(1))]).unwrap();
        assert_eq!(child, server(3));
        assert_eq!(path, [server(1), server(2), server(3)]);
        // Directly.
        let (_, path) = cycle(&edges, &[(server(2), server(1))]).unwrap();
        assert_eq!(path, [server(1), server(2)]);
    }

    #[test]
    fn a_check_is_behind_its_own_server() {
        // A server on its own check is a loop; checks of one server on
        // each other are not.
        assert!(cycle(&[], &[(server(1), check(1, "ping"))]).is_some());
        assert_eq!(cycle(&[], &[(check(1, "https"), check(1, "ping"))]), None);
        // web-1's https behind sw-core's uplink, and sw-core behind web-1:
        // web-1 as a whole is not behind its https check, so no loop.
        let edges = [edge(check(1, "https"), check(3, "uplink"))];
        assert_eq!(cycle(&edges, &[(server(3), server(1))]), None);
        let edges = [edge(server(3), server(1))];
        let (_, path) = cycle(&edges, &[(server(1), check(3, "uplink"))]).unwrap();
        assert_eq!(path, [check(3, "uplink"), server(3), server(1)]);
        // A check of server 1 on server 3's check, while server 3 is behind
        // server 1: the check is behind 1, which is fine.
        assert_eq!(
            cycle(&edges, &[(check(1, "https"), check(3, "uplink"))]),
            None
        );
    }

    #[test]
    fn automatic_edges_and_a_whole_batch_count() {
        let relay = Node::Relay(Uuid::from_u128(9));
        // web-1 behind its relay, a device of the site behind web-1.
        let edges = [Edge::automatic(server(1), relay.clone())];
        assert_eq!(cycle(&edges, &[(server(2), server(1))]), None);
        // Two of one batch that loop together.
        let adding = [(server(5), server(6)), (server(6), server(5))];
        assert!(cycle(&edges, &adding).is_some());
    }
}
