//! A map whose keys each belong to a server, grouped by it, so that what
//! the threshold engine does for one server (reload its thresholds,
//! forget it when its stream opens or closes) touches that server's
//! entries only. A flat map made each of these a scan of every server's
//! entries: with 8,000 servers reloading their thresholds at once, the
//! engine stopped for half a minute and dropped batches
//! (docs/load-test.md).

use std::collections::hash_map::{self, HashMap};
use std::hash::Hash;

use uuid::Uuid;

use super::history::PastKey;
use super::store::SeriesKey;

/// A key that names the server it belongs to.
pub trait OfServer: Eq + Hash {
    fn server_id(&self) -> Uuid;
}

impl OfServer for SeriesKey {
    fn server_id(&self) -> Uuid {
        self.server_id
    }
}

impl OfServer for PastKey {
    fn server_id(&self) -> Uuid {
        self.server_id
    }
}

impl OfServer for (Uuid, String, String) {
    fn server_id(&self) -> Uuid {
        self.0
    }
}

#[derive(Debug)]
pub struct ByServer<K, V> {
    servers: HashMap<Uuid, HashMap<K, V>>,
}

impl<K, V> Default for ByServer<K, V> {
    fn default() -> Self {
        Self {
            servers: HashMap::new(),
        }
    }
}

impl<K: OfServer, V> ByServer<K, V> {
    pub fn get(&self, key: &K) -> Option<&V> {
        self.servers.get(&key.server_id())?.get(key)
    }

    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.servers.get_mut(&key.server_id())?.get_mut(key)
    }

    pub fn contains_key(&self, key: &K) -> bool {
        self.get(key).is_some()
    }

    pub fn entry(&mut self, key: K) -> hash_map::Entry<'_, K, V> {
        self.servers.entry(key.server_id()).or_default().entry(key)
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.servers
            .entry(key.server_id())
            .or_default()
            .insert(key, value)
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        let server_id = key.server_id();
        let entries = self.servers.get_mut(&server_id)?;
        let removed = entries.remove(key);
        if entries.is_empty() {
            self.servers.remove(&server_id);
        }
        removed
    }

    /// Everything of `server_id`, gone.
    pub fn drop_server(&mut self, server_id: Uuid) {
        self.servers.remove(&server_id);
    }

    /// Keeps the entries of `server_id` for which `keep` is true; the
    /// other servers' are not looked at.
    pub fn retain_server(&mut self, server_id: Uuid, mut keep: impl FnMut(&K, &mut V) -> bool) {
        if let Some(entries) = self.servers.get_mut(&server_id) {
            entries.retain(|k, v| keep(k, v));
            if entries.is_empty() {
                self.servers.remove(&server_id);
            }
        }
    }

    /// The entries of `server_id`.
    pub fn of(&self, server_id: Uuid) -> impl Iterator<Item = (&K, &V)> {
        self.servers.get(&server_id).into_iter().flatten()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.servers.values().map(HashMap::len).sum()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }
}

impl<K: OfServer, V> Extend<(K, V)> for ByServer<K, V> {
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, items: I) {
        for (key, value) in items {
            self.insert(key, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(server: Uuid, metric: &str) -> (Uuid, String, String) {
        (server, metric.to_owned(), String::new())
    }

    #[test]
    fn entries_are_kept_and_dropped_per_server() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut map = ByServer::default();
        map.insert(key(a, "cpu"), 1);
        map.insert(key(a, "disk"), 2);
        *map.entry(key(b, "cpu")).or_default() += 3;
        assert_eq!(map.len(), 3);
        assert_eq!(map.get(&key(b, "cpu")), Some(&3));
        assert!(map.contains_key(&key(a, "disk")));

        map.retain_server(a, |k, _| k.1 == "cpu");
        assert_eq!(map.of(a).count(), 1);
        assert_eq!(map.get(&key(b, "cpu")), Some(&3), "b untouched");

        map.drop_server(b);
        assert_eq!(map.of(b).count(), 0);
        assert_eq!(map.remove(&key(a, "cpu")), Some(1));
        assert_eq!(map.len(), 0);
        assert!(map.servers.is_empty(), "no empty server left behind");
        map.extend([(key(a, "net"), 4)]);
        assert_eq!(map.get_mut(&key(a, "net")).copied(), Some(4));
    }
}
