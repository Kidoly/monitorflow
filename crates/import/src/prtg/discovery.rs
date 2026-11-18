//! PRTG auto-discovery as MonitorFlow's network scans (ADR-0154): a scan
//! per probe or group that has it on, of its address range, on its
//! schedule, with MonitorFlow's default ports. Its devices go to the
//! import's site, and so does the scan.

use crate::args;
use crate::model::{Every, Fidelity, Inventory, Origin, Scan, Unmovable};
use crate::notes::Note;
use crate::scan;

use super::tree::{Kind, Object, Tree};

/// The settings of a probe or group that say how it discovers.
pub const SETTINGS: &[&str] = &[
    "discoverytype",
    "ipselectmethod",
    "ipbase",
    "iprangestart",
    "iprangeend",
    "iplist",
    "ipcidr",
    "ipsubnet",
    "discoveryschedule",
];

/// Whether `object` has auto-discovery on: a type other than 0, manual.
pub fn discovers(object: &Object) -> bool {
    object
        .setting("discoverytype")
        .is_some_and(|t| t != "0" && t.chars().all(|c| c.is_ascii_digit()))
}

/// The address entries of `object`, or why there are none MonitorFlow
/// can scan.
fn entries(object: &Object) -> Result<Vec<String>, Note> {
    let get = |name| object.setting(name).unwrap_or_default();
    match object.setting("ipselectmethod").unwrap_or("0") {
        // A base like `192.168.0` and the last octet's range.
        "0" => {
            let base = get("ipbase").trim_end_matches('.');
            let (from, to) = (get("iprangestart"), get("iprangeend"));
            if base.is_empty() || from.is_empty() || to.is_empty() {
                return Err(Note::plain("no_address"));
            }
            Ok(vec![format!("{base}.{from}-{to}")])
        }
        "1" => Ok(get("iplist")
            .split([',', ';', '\n', '\r', ' ', '\t'])
            .filter(|e| !e.is_empty())
            .map(str::to_owned)
            .collect()),
        "2" => Ok(vec![get("ipcidr").to_owned()]),
        // An address and its mask, like `192.168.0.0/255.255.255.0`.
        "3" => Ok(vec![get("ipsubnet").to_owned()]),
        "4" => Err(Note::plain("prtg_discovery_ad")),
        _ => Err(Note::plain("no_address")),
    }
}

/// The scans of the tree's probes and groups that discover.
pub fn scans(tree: &Tree, out: &mut Inventory) {
    for object in &tree.objects {
        if !matches!(object.kind, Kind::Probe | Kind::Group) || !discovers(object) {
            continue;
        }
        let kind = if object.kind == Kind::Probe {
            "probe"
        } else {
            "group"
        };
        let path = tree.path(object.id);
        let origin = Origin::new(kind, &path).with_detail(format!(
            "discoverytype={}",
            object.setting("discoverytype").unwrap_or_default()
        ));
        let targets = match entries(object) {
            Ok(entries) => scan::targets(&entries),
            Err(why) => {
                out.unmovable.push(Unmovable {
                    hosts: vec![],
                    origin,
                    why,
                });
                continue;
            }
        };
        if targets.entries.is_empty() {
            out.unmovable.push(Unmovable {
                hosts: vec![],
                origin,
                why: targets.note().unwrap_or_else(|| Note::plain("no_address")),
            });
            continue;
        }
        let mut notes = vec![targets.note()];
        let every = if object.paused.is_some() {
            notes.insert(0, Some(Note::plain("scan_disabled")));
            None
        } else {
            match object.setting("discoveryschedule").unwrap_or("0") {
                "1" => Some(Every::Hour),
                "2" => Some(Every::Day),
                "3" => Some(Every::Week),
                "4" => {
                    notes.push(Some(Note::new("prtg_scan_monthly", args! {})));
                    Some(Every::Week)
                }
                // Once, when the group was made: when asked.
                _ => None,
            }
        };
        // PRTG adds what auto-discovery finds; a scan only proposes it.
        notes.push(Some(Note::new(
            "scan_proposes",
            args! {"action" => "PRTG auto-discovery"},
        )));
        out.scans.push(Scan {
            name: object.name.trim().to_owned(),
            targets: targets.entries,
            ports: None,
            every,
            snmp: None,
            group: Some(path),
            origin,
            fidelity: notes
                .into_iter()
                .flatten()
                .next()
                .map_or(Fidelity::Exact, Fidelity::Approximated),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::tree::Paused;
    use super::*;

    fn group(id: i64, settings: &[(&str, &str)]) -> Object {
        let mut object = Object::new(id, Kind::Group, format!("Group {id}"), Some(1));
        for (k, v) in settings {
            object.settings.insert((*k).into(), (*v).into());
        }
        object
    }

    fn read(objects: Vec<Object>) -> Inventory {
        let mut tree = Tree::default();
        tree.objects
            .push(Object::new(1, Kind::Probe, "Local Probe", None));
        tree.objects.extend(objects);
        let mut out = Inventory::default();
        scans(&tree, &mut out);
        out
    }

    #[test]
    fn groups_that_discover_become_scans() {
        let mut paused = group(
            14,
            &[
                ("discoverytype", "1"),
                ("ipselectmethod", "2"),
                ("ipcidr", "10.0.2.0/24"),
                ("discoveryschedule", "1"),
            ],
        );
        paused.paused = Some(Paused::Indefinitely);
        let out = read(vec![
            group(
                10,
                &[
                    ("discoverytype", "1"),
                    ("ipbase", "192.168.0."),
                    ("iprangestart", "1"),
                    ("iprangeend", "254"),
                    ("discoveryschedule", "2"),
                ],
            ),
            group(
                11,
                &[
                    ("discoverytype", "2"),
                    ("ipselectmethod", "1"),
                    ("iplist", "10.0.0.1\n10.0.0.2, nas.example.com"),
                    ("discoveryschedule", "0"),
                ],
            ),
            group(
                12,
                &[
                    ("discoverytype", "3"),
                    ("ipselectmethod", "3"),
                    ("ipsubnet", "10.0.1.0/255.255.255.128"),
                    ("discoveryschedule", "4"),
                ],
            ),
            group(13, &[("discoverytype", "1"), ("ipselectmethod", "4")]),
            group(15, &[("discoverytype", "0"), ("ipcidr", "10.9.0.0/24")]),
            paused,
        ]);
        let summary: Vec<_> = out
            .scans
            .iter()
            .map(|s| {
                let note = match &s.fidelity {
                    Fidelity::Exact => "exact".to_owned(),
                    Fidelity::Approximated(n) => n.code.clone(),
                };
                (s.targets.join(" "), s.every, note)
            })
            .collect();
        assert_eq!(
            summary,
            [
                (
                    "192.168.0.1-254".into(),
                    Some(Every::Day),
                    "scan_proposes".into()
                ),
                ("10.0.0.1 10.0.0.2".into(), None, "scan_skipped".into()),
                (
                    "10.0.1.0/25".into(),
                    Some(Every::Week),
                    "prtg_scan_monthly".into()
                ),
                ("10.0.2.0/24".into(), None, "scan_disabled".into()),
            ]
        );
        assert_eq!(
            out.scans[0].group.as_deref(),
            Some("Local Probe / Group 10")
        );
        assert_eq!(out.scans[0].ports, None);
        assert_eq!(out.unmovable.len(), 1);
        assert_eq!(out.unmovable[0].why.code, "prtg_discovery_ad");
    }
}
