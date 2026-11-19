//! Zabbix services and SLAs (ADR-0155) to business services.
//!
//! A service watches the problems its tags select: here, the checks whose
//! triggers carry those tags, on the hosts of the import. Child services
//! are flattened into their members, under the parent's rule: "most
//! critical of child services" is all of them, "most critical if all
//! children have problems" at least one, and an additional rule "if at
//! least N child services have a problem" at least the others. The target
//! is the SLO of the SLAs whose service tags select it.

use std::collections::{HashMap, HashSet};

use super::config::{Config, Service as ZabbixService, Sla, TagFilter};
use super::map::Tagged;
use crate::args;
use crate::model::{Fidelity, Inventory, Member, Origin, Service, Unmovable};
use crate::notes::Note;

/// Whether `tags` pass every filter of `filters`.
fn selects(filters: &[TagFilter], tags: &[(String, String)]) -> bool {
    filters
        .iter()
        .all(|f| tags.iter().any(|(t, v)| f.matches(t, v)))
}

/// The members of `service`: its own problems' checks, then its
/// children's, each once.
fn members(
    service: &ZabbixService,
    by_id: &HashMap<&str, &ZabbixService>,
    tagged: &Tagged,
    seen: &mut HashSet<String>,
    out: &mut Vec<Member>,
) {
    if !seen.insert(service.id.clone()) {
        return;
    }
    if !service.problem_tags.is_empty() {
        for (host, watches) in tagged {
            for (tags, watch) in watches {
                let member = Member {
                    host: host.clone(),
                    check: Some(watch.clone()),
                };
                if selects(&service.problem_tags, tags) && !out.contains(&member) {
                    out.push(member);
                }
            }
        }
    }
    for child in &service.children {
        if let Some(child) = by_id.get(child.as_str()) {
            members(child, by_id, tagged, seen, out);
        }
    }
}

/// The rule of `service` over `count` members, or the note saying why it
/// is not the same.
fn rule(service: &ZabbixService, count: usize) -> (Option<u32>, Option<Note>) {
    let count = u32::try_from(count).unwrap_or(u32::MAX);
    if let [rule] = service.status_rules.as_slice() {
        let n = rule.limit.max(0.0).round() as u32;
        match rule.kind {
            // A problem when at least N members have one: up while more
            // than the others are.
            0 if n >= 1 => {
                return (
                    Some(count.saturating_sub(n) + 1).filter(|&k| k < count),
                    None,
                )
            }
            // A problem when fewer than N members are OK.
            2 if n >= 1 => return (Some(n.min(count)).filter(|&k| k < count), None),
            _ => {}
        }
    }
    if !service.status_rules.is_empty() {
        let rule = service
            .status_rules
            .iter()
            .map(|r| format!("{} {}", r.kind, r.limit))
            .collect::<Vec<_>>()
            .join(", ");
        return (
            None,
            Some(Note::new("service_rule", args! {"rule" => rule})),
        );
    }
    match service.algorithm {
        _ if service.children.is_empty() => (None, None),
        1 => ((count > 1).then_some(1), None),
        2 => (None, None),
        _ => (
            None,
            Some(Note::new(
                "service_rule",
                args! {"rule" => "set status to OK"},
            )),
        ),
    }
}

/// The SLAs selecting `service`, enabled ones only.
fn slas<'a>(service: &ZabbixService, slas: &'a [Sla]) -> Vec<&'a Sla> {
    slas.iter()
        .filter(|s| {
            s.enabled && selects(&s.service_tags, &service.tags) && !s.service_tags.is_empty()
        })
        .collect()
}

/// Every service of `config` with members on the import's hosts.
pub fn services(config: &Config, tagged: &Tagged, out: &mut Inventory) {
    let by_id: HashMap<&str, &ZabbixService> =
        config.services.iter().map(|s| (s.id.as_str(), s)).collect();
    let known: HashSet<&str> = out.hosts.iter().map(|h| h.key.as_str()).collect();
    let tagged: Tagged = tagged
        .iter()
        .filter(|(host, _)| known.contains(host.as_str()))
        .map(|(h, w)| (h.clone(), w.clone()))
        .collect();
    let mut services = Vec::new();
    for service in &config.services {
        let origin = Origin::new("service", &service.name);
        let mut found = Vec::new();
        members(service, &by_id, &tagged, &mut HashSet::new(), &mut found);
        if found.is_empty() {
            out.unmovable.push(Unmovable {
                hosts: vec![],
                origin,
                why: Note::plain("service_no_members"),
            });
            continue;
        }
        let (at_least, rule_note) = rule(service, found.len());
        let selecting = slas(service, &config.slas);
        let target = selecting
            .iter()
            .map(|s| s.slo)
            .filter(|slo| (50.0..=100.0).contains(slo))
            .fold(None::<f64>, |best, slo| {
                Some(best.map_or(slo, |b| b.max(slo)))
            });
        let chosen = selecting.iter().find(|s| Some(s.slo) == target).copied();
        let note = if !service.children.is_empty() && rule_note.is_none() && !service.weighted {
            Some(Note::plain("service_children"))
        } else {
            None
        }
        .or(rule_note)
        .or_else(|| service.weighted.then(|| Note::plain("service_weights")))
        .or_else(|| {
            (selecting.len() > 1).then(|| {
                Note::new(
                    "sla_several",
                    args! {"count" => selecting.len(), "target" => target.unwrap_or_default()},
                )
            })
        })
        .or_else(|| {
            chosen
                .filter(|s| s.scheduled)
                .map(|_| Note::plain("sla_schedule"))
        })
        .or_else(|| {
            chosen
                .filter(|s| s.excluded > 0)
                .map(|_| Note::plain("sla_downtimes"))
        });
        let origin = match chosen {
            Some(sla) => origin.with_detail(format!("SLA {}: {} %", sla.name, sla.slo)),
            None => origin,
        };
        services.push(Service {
            name: service.name.clone(),
            members: found,
            at_least,
            target,
            origin,
            fidelity: note.map_or(Fidelity::Exact, Fidelity::Approximated),
        });
    }
    out.services.extend(services);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::model::{Host, Os, Watch};
    use crate::zabbix::config::StatusRule;

    fn filter(tag: &str, value: &str) -> TagFilter {
        TagFilter {
            tag: tag.into(),
            value: value.into(),
            contains: false,
        }
    }

    fn service(id: &str, name: &str, tags: Vec<TagFilter>, children: &[&str]) -> ZabbixService {
        ZabbixService {
            id: id.into(),
            name: name.into(),
            algorithm: 2,
            problem_tags: tags,
            children: children.iter().map(|c| (*c).to_owned()).collect(),
            status_rules: vec![],
            tags: vec![("sla".into(), "gold".into())],
            weighted: false,
        }
    }

    fn port(port: u16) -> Watch {
        Watch::Port {
            port,
            address: None,
        }
    }

    fn inventory() -> (Inventory, Tagged) {
        let mut out = Inventory::default();
        for key in ["mail-1", "mail-2", "web-1"] {
            out.hosts.push(Host {
                key: key.into(),
                name: key.into(),
                visible_name: None,
                addresses: vec![],
                os: Os::Debian,
                groups: vec![],
                origin: Origin::new("host", key),
                agentless: false,
            });
        }
        let tag = |v: &str| vec![("service".to_owned(), v.to_owned())];
        let tagged: Tagged = BTreeMap::from([
            (
                "mail-1".to_owned(),
                vec![(tag("smtp"), port(25)), (tag("imap"), port(993))],
            ),
            ("mail-2".to_owned(), vec![(tag("smtp"), port(25))]),
            ("web-1".to_owned(), vec![(tag("web"), port(443))]),
            ("gone".to_owned(), vec![(tag("smtp"), port(25))]),
        ]);
        (out, tagged)
    }

    fn sla(slo: f64) -> Sla {
        Sla {
            name: format!("Gold {slo}"),
            enabled: true,
            slo,
            scheduled: false,
            excluded: 0,
            service_tags: vec![filter("sla", "gold")],
        }
    }

    #[test]
    fn a_service_of_problem_tags_is_exact() {
        let (mut out, tagged) = inventory();
        let config = Config {
            services: vec![service("1", "SMTP", vec![filter("service", "smtp")], &[])],
            slas: vec![sla(99.9)],
            ..Config::default()
        };
        services(&config, &tagged, &mut out);
        let smtp = &out.services[0];
        assert_eq!(smtp.fidelity, Fidelity::Exact);
        assert_eq!(smtp.target, Some(99.9));
        let hosts: Vec<_> = smtp.members.iter().map(|m| m.host.as_str()).collect();
        // The host not in the import is left out.
        assert_eq!(hosts, ["mail-1", "mail-2"]);
        assert_eq!(smtp.members[0].check, Some(port(25)));
        assert_eq!(smtp.at_least, None);
    }

    #[test]
    fn children_are_flattened_under_the_parents_rule() {
        let (mut out, tagged) = inventory();
        let mut email = service("3", "Email", vec![], &["1", "2"]);
        email.algorithm = 1;
        let config = Config {
            services: vec![
                service("1", "SMTP", vec![filter("service", "smtp")], &[]),
                service("2", "IMAP", vec![filter("service", "imap")], &[]),
                email,
            ],
            slas: vec![sla(99.5), sla(99.95)],
            ..Config::default()
        };
        services(&config, &tagged, &mut out);
        let email = out.services.iter().find(|s| s.name == "Email").unwrap();
        assert_eq!(email.members.len(), 3);
        assert_eq!(email.at_least, Some(1));
        assert_eq!(email.target, Some(99.95));
        let Fidelity::Approximated(note) = &email.fidelity else {
            panic!("children are an approximation");
        };
        assert_eq!(note.code, "service_children");
    }

    #[test]
    fn additional_rules_and_weights() {
        let mut parent = service("9", "Web", vec![filter("service", "smtp")], &[]);
        parent.status_rules = vec![StatusRule {
            kind: 0,
            limit: 2.0,
        }];
        // Two members, a problem when 2 have one: up while at least 1 is.
        assert_eq!(rule(&parent, 2), (Some(1), None));
        assert_eq!(rule(&parent, 1).0, None);
        parent.status_rules = vec![StatusRule {
            kind: 2,
            limit: 2.0,
        }];
        assert_eq!(rule(&parent, 3), (Some(2), None));
        parent.status_rules = vec![StatusRule {
            kind: 1,
            limit: 50.0,
        }];
        assert_eq!(rule(&parent, 3).1.unwrap().code, "service_rule");
        let (mut out, tagged) = inventory();
        let mut weighted = service("1", "SMTP", vec![filter("service", "smtp")], &[]);
        weighted.weighted = true;
        let config = Config {
            services: vec![
                weighted,
                service("2", "None", vec![filter("service", "ftp")], &[]),
            ],
            ..Config::default()
        };
        services(&config, &tagged, &mut out);
        let Fidelity::Approximated(note) = &out.services[0].fidelity else {
            panic!("weights are an approximation");
        };
        assert_eq!(note.code, "service_weights");
        assert_eq!(out.unmovable[0].why.code, "service_no_members");
    }

    #[test]
    fn a_cycle_of_children_ends() {
        let (mut out, tagged) = inventory();
        let config = Config {
            services: vec![
                service("1", "A", vec![filter("service", "web")], &["2"]),
                service("2", "B", vec![], &["1"]),
            ],
            ..Config::default()
        };
        services(&config, &tagged, &mut out);
        assert_eq!(out.services.len(), 2);
        assert!(out.services.iter().all(|s| s.members.len() == 1));
    }
}
