//! The hardware and software inventory's shared rules (ADR-0187): its
//! canonical form and hash, the changes between two inventories, and the
//! bounds an agent keeps to and a server enforces. Agent and server apply
//! the same functions, so the hash an agent sends is the hash the server
//! computes from what it stored.

use std::collections::BTreeMap;

use prost::Message;
use ring::digest::{digest, SHA256};

use crate::proto::v1::{
    Inventory, InventoryDelta, InventoryField, InventoryItem, InventoryItemKey,
};

/// The kinds of item, in the order the web interface shows them. Items of
/// another kind are dropped by [`bound`].
pub const KINDS: [&str; 9] = [
    "system",
    "processor",
    "memory",
    "disk",
    "nic",
    "software",
    "service",
    "feature",
    "licence",
];

/// Items kept at most; past it the last ones in canonical order go.
pub const MAX_ITEMS: usize = 4000;
/// Longest key, in bytes.
pub const MAX_KEY: usize = 256;
/// Fields kept per item at most.
pub const MAX_FIELDS: usize = 24;
/// Longest field name, in bytes.
pub const MAX_FIELD_NAME: usize = 32;
/// Longest field value, in bytes; longer values are cut on a character
/// boundary.
pub const MAX_VALUE: usize = 512;

/// The length of a hash.
pub const HASH_LEN: usize = 32;

/// `inventory` sorted (items by kind then key, fields by name), without
/// empty fields, and with the first of duplicate items and fields only.
pub fn canonical(inventory: Inventory) -> Inventory {
    let mut items: BTreeMap<(String, String), BTreeMap<String, String>> = BTreeMap::new();
    for item in inventory.items {
        items.entry((item.kind, item.key)).or_insert_with(|| {
            let mut fields = BTreeMap::new();
            for field in item.fields {
                if !field.value.is_empty() {
                    fields.entry(field.name).or_insert(field.value);
                }
            }
            fields
        });
    }
    Inventory {
        items: items
            .into_iter()
            .map(|((kind, key), fields)| InventoryItem {
                kind,
                key,
                fields: fields
                    .into_iter()
                    .map(|(name, value)| InventoryField { name, value })
                    .collect(),
            })
            .collect(),
    }
}

/// SHA-256 of the protobuf encoding of `inventory`, which must be
/// canonical.
pub fn hash(inventory: &Inventory) -> Vec<u8> {
    digest(&SHA256, &inventory.encode_to_vec())
        .as_ref()
        .to_vec()
}

/// What makes `from` into `to`, both canonical.
pub fn delta(from: &Inventory, to: &Inventory) -> InventoryDelta {
    let before: BTreeMap<(&str, &str), &InventoryItem> = from
        .items
        .iter()
        .map(|i| ((i.kind.as_str(), i.key.as_str()), i))
        .collect();
    let after: BTreeMap<(&str, &str), &InventoryItem> = to
        .items
        .iter()
        .map(|i| ((i.kind.as_str(), i.key.as_str()), i))
        .collect();
    InventoryDelta {
        base_hash: hash(from),
        upserted: to
            .items
            .iter()
            .filter(|i| before.get(&(i.kind.as_str(), i.key.as_str())) != Some(i))
            .cloned()
            .collect(),
        removed: from
            .items
            .iter()
            .filter(|i| !after.contains_key(&(i.kind.as_str(), i.key.as_str())))
            .map(|i| InventoryItemKey {
                kind: i.kind.clone(),
                key: i.key.clone(),
            })
            .collect(),
    }
}

/// `base` with `delta` applied, canonical. The caller checks
/// `delta.base_hash` against `base` first.
pub fn apply(base: &Inventory, delta: &InventoryDelta) -> Inventory {
    let mut items: BTreeMap<(String, String), InventoryItem> = base
        .items
        .iter()
        .map(|i| ((i.kind.clone(), i.key.clone()), i.clone()))
        .collect();
    for gone in &delta.removed {
        items.remove(&(gone.kind.clone(), gone.key.clone()));
    }
    for item in &delta.upserted {
        items.insert((item.kind.clone(), item.key.clone()), item.clone());
    }
    canonical(Inventory {
        items: items.into_values().collect(),
    })
}

/// One item's change between two inventories.
#[derive(Debug, Clone, PartialEq)]
pub enum Change<'a> {
    Added(&'a InventoryItem),
    Removed(&'a InventoryItem),
    Changed {
        before: &'a InventoryItem,
        after: &'a InventoryItem,
    },
}

/// Every item that differs between `from` and `to`, both canonical, in
/// canonical order.
pub fn changes<'a>(from: &'a Inventory, to: &'a Inventory) -> Vec<Change<'a>> {
    let mut before = from.items.iter().peekable();
    let mut after = to.items.iter().peekable();
    let mut out = Vec::new();
    loop {
        let order = match (before.peek(), after.peek()) {
            (None, None) => return out,
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(b), Some(a)) => (&b.kind, &b.key).cmp(&(&a.kind, &a.key)),
        };
        match order {
            std::cmp::Ordering::Less => out.push(Change::Removed(before.next().expect("peeked"))),
            std::cmp::Ordering::Greater => out.push(Change::Added(after.next().expect("peeked"))),
            std::cmp::Ordering::Equal => {
                let (b, a) = (
                    before.next().expect("peeked"),
                    after.next().expect("peeked"),
                );
                if b != a {
                    out.push(Change::Changed {
                        before: b,
                        after: a,
                    });
                }
            }
        }
    }
}

/// `inventory` made canonical and cut to the bounds above: items of an
/// unknown kind, with an empty or too long key, or holding a NUL anywhere
/// are dropped, as are fields with a bad name; long values are cut. Also
/// returns how many items were dropped.
pub fn bound(inventory: Inventory) -> (Inventory, usize) {
    let inventory = canonical(inventory);
    let total = inventory.items.len();
    let mut items: Vec<InventoryItem> = inventory
        .items
        .into_iter()
        .filter(|i| KINDS.contains(&i.kind.as_str()))
        .filter(|i| !i.key.is_empty() && i.key.len() <= MAX_KEY && !i.key.contains('\0'))
        .map(|mut item| {
            item.fields.retain(|f| {
                !f.name.is_empty()
                    && f.name.len() <= MAX_FIELD_NAME
                    && f.name
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                    && !f.value.contains('\0')
            });
            item.fields.truncate(MAX_FIELDS);
            for field in &mut item.fields {
                cut(&mut field.value, MAX_VALUE);
            }
            item
        })
        .collect();
    items.truncate(MAX_ITEMS);
    let dropped = total - items.len();
    (Inventory { items }, dropped)
}

/// `text` cut to at most `max` bytes, on a character boundary.
fn cut(text: &mut String, max: usize) {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
}

/// An item's fields as a map, for storage and display.
pub fn fields(item: &InventoryItem) -> BTreeMap<&str, &str> {
    item.fields
        .iter()
        .map(|f| (f.name.as_str(), f.value.as_str()))
        .collect()
}

/// A builder of items, for collectors.
#[derive(Debug, Default)]
pub struct Items(Vec<InventoryItem>);

impl Items {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an item of `kind` keyed `key` with the non-empty `fields`.
    pub fn push<'a>(
        &mut self,
        kind: &str,
        key: impl Into<String>,
        fields: impl IntoIterator<Item = (&'a str, String)>,
    ) {
        let key = key.into();
        let key = key.trim();
        if key.is_empty() {
            return;
        }
        self.0.push(InventoryItem {
            kind: kind.to_owned(),
            key: key.to_owned(),
            fields: fields
                .into_iter()
                .map(|(name, value)| InventoryField {
                    name: name.to_owned(),
                    value: value.trim().to_owned(),
                })
                .filter(|f| !f.value.is_empty())
                .collect(),
        });
    }

    /// The canonical, bounded inventory.
    pub fn finish(self) -> Inventory {
        bound(Inventory { items: self.0 }).0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: &str, key: &str, fields: &[(&str, &str)]) -> InventoryItem {
        InventoryItem {
            kind: kind.into(),
            key: key.into(),
            fields: fields
                .iter()
                .map(|(n, v)| InventoryField {
                    name: (*n).into(),
                    value: (*v).into(),
                })
                .collect(),
        }
    }

    #[test]
    fn canonical_sorts_and_dedupes() {
        let messy = Inventory {
            items: vec![
                item("software", "Zed", &[("version", "1")]),
                item(
                    "disk",
                    "sda",
                    &[("size", "10"), ("model", "X"), ("empty", "")],
                ),
                item("disk", "sda", &[("size", "20")]),
                item("disk", "nvme0n1", &[("model", "Y"), ("model", "Z")]),
            ],
        };
        let canonical = canonical(messy);
        assert_eq!(
            canonical.items,
            vec![
                item("disk", "nvme0n1", &[("model", "Y")]),
                item("disk", "sda", &[("model", "X"), ("size", "10")]),
                item("software", "Zed", &[("version", "1")]),
            ]
        );
        // The same items in another order hash the same.
        let shuffled = Inventory {
            items: canonical.items.iter().rev().cloned().collect(),
        };
        assert_eq!(hash(&super::canonical(shuffled)), hash(&canonical));
        assert_eq!(hash(&canonical).len(), HASH_LEN);
    }

    #[test]
    fn a_delta_rebuilds_the_new_inventory() {
        let before = canonical(Inventory {
            items: vec![
                item("disk", "sda", &[("model", "X")]),
                item("software", "7-Zip", &[("version", "23.01")]),
                item("software", "Old", &[("version", "1")]),
            ],
        });
        let after = canonical(Inventory {
            items: vec![
                item("disk", "sda", &[("model", "X")]),
                item("software", "7-Zip", &[("version", "24.08")]),
                item("software", "New", &[("version", "2")]),
            ],
        });
        let delta = delta(&before, &after);
        assert_eq!(delta.base_hash, hash(&before));
        assert_eq!(delta.upserted.len(), 2, "the unchanged disk is not sent");
        assert_eq!(delta.removed.len(), 1);
        let rebuilt = apply(&before, &delta);
        assert_eq!(rebuilt, after);
        assert_eq!(hash(&rebuilt), hash(&after));

        let found = changes(&before, &after);
        assert_eq!(found.len(), 3);
        assert!(matches!(found[0], Change::Changed { before, .. } if before.key == "7-Zip"));
        assert!(matches!(found[1], Change::Added(i) if i.key == "New"));
        assert!(matches!(found[2], Change::Removed(i) if i.key == "Old"));
        assert!(changes(&after, &after).is_empty());
    }

    #[test]
    fn bound_drops_what_does_not_fit() {
        let long = "é".repeat(400);
        let (bounded, dropped) = bound(Inventory {
            items: vec![
                item("gadget", "x", &[]),
                item("disk", "", &[]),
                item("disk", "a\0b", &[]),
                item("disk", &"k".repeat(MAX_KEY + 1), &[]),
                item(
                    "disk",
                    "sda",
                    &[("Model", "x"), ("model", &long), ("bad", "a\0")],
                ),
            ],
        });
        assert_eq!(dropped, 4);
        assert_eq!(bounded.items.len(), 1);
        let fields = &bounded.items[0].fields;
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "model");
        assert!(fields[0].value.len() <= MAX_VALUE);
        assert!(fields[0].value.chars().all(|c| c == 'é'));

        let many = Inventory {
            items: (0..MAX_ITEMS + 5)
                .map(|n| item("software", &format!("p{n:05}"), &[]))
                .collect(),
        };
        let (bounded, dropped) = bound(many);
        assert_eq!((bounded.items.len(), dropped), (MAX_ITEMS, 5));
    }

    #[test]
    fn the_builder_trims_and_skips_empty_values() {
        let mut items = Items::new();
        items.push(
            "system",
            "system",
            [("model", " X1 ".into()), ("serial", "".into())],
        );
        items.push("disk", "  ", [("model", "lost".into())]);
        let inventory = items.finish();
        assert_eq!(
            inventory.items,
            vec![item("system", "system", &[("model", "X1")])]
        );
    }
}
