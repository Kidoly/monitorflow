//! Custom SNMP checks on the server (ADR-0157): the rows of kind
//! `snmp_custom` name lookups by reference (`lookupN`), a built-in one
//! (`builtin:<name>`) or a customer's own by ID, and the server expands
//! them into `lookupN_map` when it sends the check to a probe.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use monitorflow_snmp::custom::{self, Custom, Lookup};
use monitorflow_snmp::library;
use sqlx::PgConnection;
use uuid::Uuid;

/// Most parameters of a custom check: the device's few and eleven per
/// channel.
pub const MAX_PARAMS: usize = 3 + custom::MAX_CHANNELS * 11;

/// A check's reference to a lookup.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum LookupRef {
    Builtin(String),
    Own(Uuid),
}

impl LookupRef {
    pub fn parse(value: &str) -> Result<Self, String> {
        let value = value.trim();
        if value.starts_with("builtin:") {
            return library::builtin_lookup(value)
                .map(|l| Self::Builtin(l.id.to_owned()))
                .ok_or_else(|| format!("there is no built-in lookup `{}`", value.escape_debug()));
        }
        Uuid::parse_str(value)
            .map(Self::Own)
            .map_err(|_| "a lookup is `builtin:<name>` or the ID of a lookup".to_string())
    }
}

/// The lookups `params` names, by channel number.
pub fn lookup_refs(
    params: &BTreeMap<String, String>,
) -> Result<BTreeMap<usize, LookupRef>, String> {
    let mut refs = BTreeMap::new();
    for (key, value) in params {
        let Some(n) = key
            .strip_prefix("lookup")
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|n| n.parse::<usize>().ok())
        else {
            continue;
        };
        if value.trim().is_empty() {
            continue;
        }
        refs.insert(
            n,
            LookupRef::parse(value).map_err(|e| format!("param `{key}`: {e}"))?,
        );
    }
    Ok(refs)
}

/// A lookup that stands for any other while a row is checked without the
/// database.
fn placeholder() -> Lookup {
    library::builtin_lookups().remove(0).lookup
}

/// Checks the parameters of a custom check as a row holds them: no
/// credentials and no expanded lookups, references that read, and
/// channels as the probe reads them. `address` is the device's.
pub fn check_row(
    params: &BTreeMap<String, String>,
    address: Option<&str>,
) -> Result<Custom, String> {
    if let Some(key) = params
        .keys()
        .find(|k| monitorflow_snmp::CREDENTIAL_PARAMS.contains(&k.as_str()))
    {
        return Err(format!(
            "param `{key}` is not for a check: set a device's SNMP credentials with PUT /api/devices/{{id}}/snmp"
        ));
    }
    if let Some(key) = params.keys().find(|k| k.ends_with("_map")) {
        return Err(format!(
            "param `{key}` is not for a check: name a lookup with `lookupN`"
        ));
    }
    let refs = lookup_refs(params)?;
    let mut checked = params.clone();
    checked
        .entry("host".into())
        .or_insert_with(|| address.unwrap_or("192.0.2.1").to_owned());
    checked.insert("community".into(), "checked".into());
    for (n, reference) in &refs {
        let lookup = match reference {
            LookupRef::Builtin(id) => library::builtin_lookup(id).map(|l| l.lookup),
            LookupRef::Own(_) => None,
        }
        .unwrap_or_else(placeholder);
        checked.insert(
            format!("lookup{n}_map"),
            serde_json::to_string(&lookup).unwrap_or_default(),
        );
    }
    monitorflow_snmp::device(&checked)?;
    custom::parse(&checked)
}

/// Whether each lookup of `refs` is built in or one of `customer`'s.
pub async fn check_owned(
    db: &mut PgConnection,
    refs: &BTreeMap<usize, LookupRef>,
    customer: Uuid,
) -> Result<Result<(), String>, sqlx::Error> {
    let ids: Vec<Uuid> = refs
        .values()
        .filter_map(|r| match r {
            LookupRef::Own(id) => Some(*id),
            LookupRef::Builtin(_) => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if ids.is_empty() {
        return Ok(Ok(()));
    }
    let found: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM snmp_lookups WHERE id = ANY($1) AND customer_id = $2")
            .bind(&ids)
            .bind(customer)
            .fetch_all(&mut *db)
            .await?;
    Ok(match ids.iter().find(|id| !found.contains(id)) {
        Some(id) => Err(format!("there is no lookup {id}")),
        None => Ok(()),
    })
}

/// The stored lookups `ids`, by ID; one whose definition does not read is
/// left out, logged.
pub async fn load_lookups(
    db: &mut PgConnection,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, Lookup>, sqlx::Error> {
    let rows: Vec<(Uuid, serde_json::Value)> =
        sqlx::query_as("SELECT id, definition FROM snmp_lookups WHERE id = ANY($1)")
            .bind(ids)
            .fetch_all(&mut *db)
            .await?;
    Ok(rows
        .into_iter()
        .filter_map(
            |(id, definition)| match serde_json::from_value::<Lookup>(definition) {
                Ok(lookup) => Some((id, lookup)),
                Err(error) => {
                    tracing::warn!(lookup = %id, %error, "a stored lookup does not read");
                    None
                }
            },
        )
        .collect())
}

/// Adds to `params` the definition of each lookup it names, as
/// `lookupN_map`, from `stored` for a customer's own. Errors name a
/// lookup that is gone.
pub fn expand(
    params: &mut BTreeMap<String, String>,
    stored: &HashMap<Uuid, Lookup>,
) -> Result<(), String> {
    for (n, reference) in lookup_refs(params)? {
        let lookup = match &reference {
            LookupRef::Builtin(id) => library::builtin_lookup(id).map(|l| l.lookup),
            LookupRef::Own(id) => stored.get(id).cloned(),
        }
        .ok_or_else(|| format!("the lookup of channel {n} is gone"))?;
        let map = serde_json::to_string(&lookup).map_err(|e| e.to_string())?;
        params.insert(format!("lookup{n}_map"), map);
    }
    Ok(())
}

/// The IDs of a customer's own lookups `params` names.
pub fn own_ids(params: &BTreeMap<String, String>) -> Vec<Uuid> {
    lookup_refs(params)
        .unwrap_or_default()
        .into_values()
        .filter_map(|r| match r {
            LookupRef::Own(id) => Some(id),
            LookupRef::Builtin(_) => None,
        })
        .collect()
}

/// The metric standing for a custom check on the server page: its first
/// channel's value, or its state for a text or a lookup; and whether it
/// is a state.
pub fn primary_suffix(params: &BTreeMap<String, String>) -> Option<(String, bool)> {
    let custom = check_row(params, None).ok()?;
    let first = custom.channels.first()?;
    let state = first.kind == custom::ValueKind::Text || first.lookup.is_some();
    Some(if state {
        (format!("{}_state", first.key), true)
    } else {
        (first.key.clone(), false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn rows_name_lookups_and_hold_no_secrets() {
        let id = Uuid::new_v4().to_string();
        let row = params(&[
            ("oid1", "1.3.6.1.4.1.6574.1.1.0"),
            ("name1", "System"),
            ("lookup1", "builtin:ok_warning_failed"),
            ("oid2", "1.3.6.1.4.1.6574.1.5.0"),
            ("name2", "Power"),
            ("lookup2", &id),
        ]);
        let custom = check_row(&row, Some("192.0.2.4")).unwrap();
        assert_eq!(custom.channels.len(), 2);
        let refs = lookup_refs(&row).unwrap();
        assert_eq!(
            refs[&1],
            LookupRef::Builtin("builtin:ok_warning_failed".into())
        );
        assert_eq!(own_ids(&row), vec![Uuid::parse_str(&id).unwrap()]);
        for (bad, error) in [
            (("community", "public"), "PUT /api/devices"),
            (("lookup1_map", "{}"), "name a lookup"),
            (("lookup1", "builtin:nope"), "no built-in lookup"),
            (("lookup1", "mine"), "the ID of a lookup"),
            (("oid1", "sysName"), "numeric OID"),
        ] {
            let mut wrong = row.clone();
            wrong.insert(bad.0.into(), bad.1.into());
            let got = check_row(&wrong, None).unwrap_err();
            assert!(got.contains(error), "{bad:?}: {got}");
            assert!(!got.contains("public"));
        }
    }

    #[test]
    fn lookups_are_expanded_for_the_probe() {
        let id = Uuid::new_v4();
        let mut row = params(&[
            ("host", "192.0.2.4"),
            ("community", "c"),
            ("oid1", "1.3.6.1.4.1.6574.1.1.0"),
            ("lookup1", "builtin:truth_value"),
            ("oid2", "1.3.6.1.4.1.6574.1.5.0"),
            ("lookup2", &id.to_string()),
        ]);
        let mine = library::lookup_from(&[(1, "on".into()), (2, "failed".into())]);
        assert!(expand(&mut row.clone(), &HashMap::new())
            .unwrap_err()
            .contains("channel 2"));
        expand(&mut row, &HashMap::from([(id, mine.clone())])).unwrap();
        let custom = monitorflow_snmp::custom::parse(&row).unwrap();
        assert_eq!(custom.channels[1].lookup.as_ref(), Some(&mine));
        assert_eq!(
            custom.channels[0].lookup.as_ref().unwrap().entries[0].name,
            "True"
        );
    }

    #[test]
    fn the_first_channel_stands_for_the_check() {
        let gauge = params(&[("oid1", "1.3.6.1.4.1.1.0"), ("name1", "Sessions")]);
        assert_eq!(primary_suffix(&gauge), Some(("sessions".into(), false)));
        let text = params(&[
            ("oid1", "1.3.6.1.4.1.1.0"),
            ("name1", "Firmware"),
            ("type1", "text"),
        ]);
        assert_eq!(primary_suffix(&text), Some(("firmware_state".into(), true)));
        let state = params(&[
            ("oid1", "1.3.6.1.4.1.1.0"),
            ("key1", "raid"),
            ("lookup1", "builtin:if_status"),
        ]);
        assert_eq!(primary_suffix(&state), Some(("raid_state".into(), true)));
        assert_eq!(primary_suffix(&BTreeMap::new()), None);
    }
}
