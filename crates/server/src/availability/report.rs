//! A report from [`Data`]: the rules of ADR-0155 applied to each server,
//! device, check and business service, with nothing read from elsewhere.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use super::data::{Data, Incident, Machine, Silence};
use super::{combine, error_budget, measure, Measured, Outage, Source, Span, Spans, Timeline};
use crate::notify::silence::{check_covers, CheckRef};

/// The prefix of every metric agents write.
const METRIC_PREFIX: &str = "monitorflow_";

/// What made something down for a while.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[schema(as = AvailabilityCause)]
pub struct Cause {
    /// `check`: an incident of a check; `offline`: the server stopped
    /// reporting; `unreachable`: what it is reached through was down
    /// (`via`); `member`: a member of a business service was down.
    pub kind: String,
    /// The server or device.
    pub server_id: Uuid,
    pub server_name: String,
    /// The check, for `check` and for a `member` that is a check.
    pub check: Option<String>,
    /// What the check watched, like a mount point; `""` when nothing.
    pub instance: String,
    /// What it depends on, for `unreachable`: a relay's name.
    pub via: Option<String>,
}

/// One outage.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[schema(as = AvailabilityOutage)]
pub struct OutageView {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub seconds: f64,
    /// Whether it was still going on when the report was made.
    pub ongoing: bool,
    /// What was down, the earliest first.
    pub causes: Vec<Cause>,
}

/// The numbers of one thing over the period.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[schema(as = AvailabilityNumbers)]
pub struct Numbers {
    /// Percentage of the counted time that was up, 0 to 100; `null` when
    /// nothing was counted (all of it planned maintenance).
    pub availability: Option<f64>,
    /// The period without planned maintenance, seconds.
    pub counted_seconds: f64,
    pub down_seconds: f64,
    /// Planned maintenance, left out of the count, seconds.
    pub maintenance_seconds: f64,
    pub outages: usize,
    pub longest_seconds: f64,
}

impl Numbers {
    fn of<C>(measured: &Measured<C>) -> Self {
        Self {
            availability: measured.availability().map(|a| a * 100.0),
            counted_seconds: measured.counted,
            down_seconds: measured.down,
            maintenance_seconds: measured.maintenance,
            outages: measured.outages.len(),
            longest_seconds: measured.longest(),
        }
    }
}

/// A server or device.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[schema(as = AvailabilityMachine)]
pub struct MachineView {
    pub id: Uuid,
    pub name: String,
    /// A device, watched from a probe, rather than a server with an agent.
    pub device: bool,
    pub site_id: Uuid,
    pub site_name: String,
    pub customer_id: Uuid,
    pub numbers: Numbers,
    pub outage_list: Vec<OutageView>,
}

/// A check that had outages.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[schema(as = AvailabilityCheck)]
pub struct CheckView {
    pub server_id: Uuid,
    pub server_name: String,
    pub name: String,
    pub instance: String,
    pub numbers: Numbers,
    pub outage_list: Vec<OutageView>,
}

/// A member of a business service, by name.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[schema(as = AvailabilityMember)]
pub struct MemberView {
    pub server_id: Uuid,
    pub server_name: String,
    pub check: Option<CheckRef>,
}

/// A business service.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[schema(as = AvailabilityService)]
pub struct ServiceView {
    pub id: Uuid,
    pub name: String,
    pub customer_id: Uuid,
    /// `all` or `at_least`.
    pub rule: String,
    pub at_least: Option<u32>,
    /// The SLA target, a percentage; `null` without one.
    pub target: Option<f64>,
    /// Whether the target was met; `null` without a target or when
    /// nothing was counted.
    pub met: Option<bool>,
    /// The down time the target allowed, minus the down time there was,
    /// seconds; negative when it was exceeded.
    pub budget_seconds: Option<f64>,
    pub members: Vec<MemberView>,
    pub numbers: Numbers,
    pub outage_list: Vec<OutageView>,
}

/// Planned maintenance during the period.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[schema(as = AvailabilityMaintenance)]
pub struct MaintenanceView {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub reason: String,
    /// What it covered: servers and devices of the report, by name.
    pub servers: Vec<String>,
    /// Only these checks of them; empty for all of them.
    pub checks: Vec<CheckRef>,
    /// An occurrence of a maintenance window.
    pub window: bool,
}

/// The overall line of a report.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[schema(as = AvailabilitySummary)]
pub struct Summary {
    pub services: usize,
    pub services_met: usize,
    pub services_missed: usize,
    pub machines: usize,
    /// Outages of servers and devices, and their down time in all.
    pub outages: usize,
    pub down_seconds: f64,
    /// Planned maintenance of the period, its own time once.
    pub maintenance_seconds: f64,
}

/// A whole report.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[schema(as = AvailabilityReport)]
pub struct Report {
    /// The customer it is for; `null` for everything the reader sees.
    pub customer_id: Option<Uuid>,
    pub customer_name: Option<String>,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub generated_at: DateTime<Utc>,
    pub summary: Summary,
    pub services: Vec<ServiceView>,
    pub machines: Vec<MachineView>,
    /// Only checks that had outages; the others were up all along.
    pub checks: Vec<CheckView>,
    pub maintenance: Vec<MaintenanceView>,
}

/// The check that wrote `metric`: the longest known name that is the
/// metric or prefixes it with `_`, else the metric itself.
pub fn check_of(metric: &str, names: &[String]) -> String {
    let short = metric.strip_prefix(METRIC_PREFIX).unwrap_or(metric);
    names
        .iter()
        .filter(|name| {
            short == name.as_str()
                || short
                    .strip_prefix(name.as_str())
                    .is_some_and(|rest| rest.starts_with('_'))
        })
        .max_by_key(|name| name.len())
        .cloned()
        .unwrap_or_else(|| short.to_owned())
}

/// Whether `silence` covers all of `machine`.
fn whole(silence: &Silence, machine: &Machine) -> bool {
    silence.checks.is_none()
        && match (
            silence.server_id,
            silence.relay_id,
            silence.site_id,
            silence.customer_id,
        ) {
            (Some(server), ..) => server == machine.id,
            (_, Some(_), ..) => false,
            (_, _, Some(site), _) => site == machine.site_id,
            (.., Some(customer)) => customer == machine.customer_id,
            _ => true,
        }
}

/// Whether `silence` names checks of `machine` (of it, or of its site).
fn names_checks_of<'s>(silence: &'s Silence, machine: &Machine) -> Option<&'s [CheckRef]> {
    let checks = silence.checks.as_deref()?;
    (silence.server_id == Some(machine.id) || silence.site_id == Some(machine.site_id))
        .then_some(checks)
}

struct Builder<'a> {
    data: &'a Data,
    period: Span,
    now: DateTime<Utc>,
}

impl<'a> Builder<'a> {
    fn machine(&self, id: Uuid) -> Option<&'a Machine> {
        self.data.machines.iter().find(|m| m.id == id)
    }

    /// When `machine` existed within the period, if at all.
    fn lifetime(&self, machine: &Machine) -> Option<Span> {
        let start = self.period.0.max(machine.created_at);
        let mut end = self.period.1.min(self.now);
        if let Some(revoked) = machine.revoked_at {
            end = end.min(revoked);
        }
        (start < end).then_some((start, end))
    }

    fn spans(&self, keep: impl Fn(&Silence) -> bool) -> Spans {
        Spans::from_spans(
            self.data
                .silences
                .iter()
                .filter(|s| keep(s))
                .map(|s| s.span),
        )
    }

    fn cause(&self, machine: &Machine, incident: &Incident) -> Cause {
        let offline = incident.metric == "offline";
        Cause {
            kind: if offline { "offline" } else { "check" }.into(),
            server_id: machine.id,
            server_name: machine.name.clone(),
            check: (!offline).then(|| check_of(&incident.metric, &self.data.names)),
            instance: incident.instance.clone(),
            via: None,
        }
    }

    /// The incidents of `machine` that `keep` takes, each excused by the
    /// silences that cover it, and the offline time of its relays.
    fn sources(&self, machine: &Machine, keep: impl Fn(&Incident) -> bool) -> Vec<Source<Cause>> {
        let names = &self.data.names;
        let mut sources: Vec<Source<Cause>> = self
            .data
            .incidents
            .iter()
            .filter(|i| i.server_id == Some(machine.id) && keep(i))
            .map(|incident| {
                let offline = incident.metric == "offline";
                let cover = self.spans(|s| {
                    whole(s, machine)
                        || (!offline
                            && names_checks_of(s, machine).is_some_and(|checks| {
                                checks.iter().any(|c| {
                                    check_covers(c, names, &incident.metric, &incident.instance)
                                })
                            }))
                });
                Source {
                    down: incident.critical.clone(),
                    cover,
                    cause: self.cause(machine, incident),
                }
            })
            .collect();
        for relay in &machine.relays {
            let cover = self
                .spans(|s| whole(s, machine) || (s.relay_id == Some(*relay) && s.checks.is_none()));
            for incident in self
                .data
                .incidents
                .iter()
                .filter(|i| i.relay_id == Some(*relay))
            {
                sources.push(Source {
                    down: incident.critical.clone(),
                    cover: cover.clone(),
                    cause: Cause {
                        kind: "unreachable".into(),
                        server_id: machine.id,
                        server_name: machine.name.clone(),
                        check: None,
                        instance: String::new(),
                        via: self.data.relays.get(relay).cloned(),
                    },
                });
            }
        }
        sources
    }

    fn measure_machine(&self, machine: &Machine) -> Option<Measured<Cause>> {
        let life = self.lifetime(machine)?;
        let maintenance = self.spans(|s| whole(s, machine));
        Some(measure(
            life,
            &maintenance,
            &self.sources(machine, |_| true),
        ))
    }

    fn measure_check(&self, machine: &Machine, check: &CheckRef) -> Option<Measured<Cause>> {
        let life = self.lifetime(machine)?;
        let names = &self.data.names;
        let maintenance = self.spans(|s| {
            whole(s, machine)
                || names_checks_of(s, machine).is_some_and(|checks| {
                    checks.iter().any(|c| {
                        c.name == check.name
                            && (c.instance.is_empty() || c.instance == check.instance)
                    })
                })
        });
        let sources = self.sources(machine, |i| {
            i.metric == "offline" || check_covers(check, names, &i.metric, &i.instance)
        });
        Some(measure(life, &maintenance, &sources))
    }

    fn outages(&self, outages: &[Outage<Cause>]) -> Vec<OutageView> {
        let end = self.period.1.min(self.now);
        outages
            .iter()
            .map(|o| OutageView {
                start: o.start,
                end: o.end,
                seconds: o.seconds(),
                ongoing: o.end >= end && self.now < self.period.1,
                causes: o.causes.clone(),
            })
            .collect()
    }
}

/// The report of `data` over `period`, made at `now`.
pub fn build(data: &Data, period: Span, now: DateTime<Utc>) -> Report {
    let b = Builder { data, period, now };
    let mut machines = Vec::new();
    let mut checks = Vec::new();
    let mut down_total = 0.0;
    let mut outage_total = 0;
    for machine in &data.machines {
        let Some(measured) = b.measure_machine(machine) else {
            continue;
        };
        down_total += measured.down;
        outage_total += measured.outages.len();
        machines.push(MachineView {
            id: machine.id,
            name: machine.name.clone(),
            device: machine.device,
            site_id: machine.site_id,
            site_name: machine.site_name.clone(),
            customer_id: machine.customer_id,
            numbers: Numbers::of(&measured),
            outage_list: b.outages(&measured.outages),
        });
        let mut names: Vec<String> = data
            .incidents
            .iter()
            .filter(|i| i.server_id == Some(machine.id) && i.metric != "offline")
            .map(|i| check_of(&i.metric, &data.names))
            .collect();
        names.sort_unstable();
        names.dedup();
        for name in names {
            let check = CheckRef {
                name,
                instance: String::new(),
            };
            let Some(measured) = b.measure_check(machine, &check) else {
                continue;
            };
            let own = measured
                .outages
                .iter()
                .any(|o| o.causes.iter().any(|c| c.kind == "check"));
            if own {
                checks.push(CheckView {
                    server_id: machine.id,
                    server_name: machine.name.clone(),
                    name: check.name,
                    instance: check.instance,
                    numbers: Numbers::of(&measured),
                    outage_list: b.outages(&measured.outages),
                });
            }
        }
    }
    let services = services(&b);
    let maintenance = maintenance(&b);
    let maintenance_seconds =
        Spans::from_spans(maintenance.iter().map(|m| (m.start, m.end))).seconds();
    Report {
        customer_id: None,
        customer_name: None,
        from: period.0,
        to: period.1,
        generated_at: now,
        summary: Summary {
            services: services.len(),
            services_met: services.iter().filter(|s| s.met == Some(true)).count(),
            services_missed: services.iter().filter(|s| s.met == Some(false)).count(),
            machines: machines.len(),
            outages: outage_total,
            down_seconds: down_total,
            maintenance_seconds,
        },
        services,
        machines,
        checks,
        maintenance,
    }
}

fn services(b: &Builder) -> Vec<ServiceView> {
    let span = (b.period.0, b.period.1.min(b.now));
    let mut views = Vec::new();
    if span.0 >= span.1 {
        return views;
    }
    for service in &b.data.services {
        let mut members = Vec::new();
        let mut timelines = Vec::new();
        for member in &service.members {
            let Some(machine) = b.machine(member.server_id) else {
                continue;
            };
            if machine.customer_id != service.customer_id {
                continue;
            }
            let measured = match &member.check {
                None => b.measure_machine(machine),
                Some(check) => b.measure_check(machine, check),
            };
            timelines.push(
                measured
                    .map(|m| m.timeline)
                    .unwrap_or_else(Timeline::default),
            );
            members.push(MemberView {
                server_id: machine.id,
                server_name: machine.name.clone(),
                check: member.check.clone(),
            });
        }
        let measured = combine(span, service.rule, &timelines);
        let outages: Vec<Outage<Cause>> = measured
            .outages
            .iter()
            .map(|o| Outage {
                start: o.start,
                end: o.end,
                causes: o
                    .causes
                    .iter()
                    .map(|&i| Cause {
                        kind: "member".into(),
                        server_id: members[i].server_id,
                        server_name: members[i].server_name.clone(),
                        check: members[i].check.as_ref().map(|c| c.name.clone()),
                        instance: members[i]
                            .check
                            .as_ref()
                            .map(|c| c.instance.clone())
                            .unwrap_or_default(),
                        via: None,
                    })
                    .collect(),
            })
            .collect();
        let availability = measured.availability().map(|a| a * 100.0);
        let met = service
            .target
            .zip(availability)
            .map(|(target, a)| a >= target - 1e-9);
        let (rule, at_least) = match service.rule {
            super::Rule::All => ("all", None),
            super::Rule::AtLeast(n) => ("at_least", Some(n)),
        };
        views.push(ServiceView {
            id: service.id,
            name: service.name.clone(),
            customer_id: service.customer_id,
            rule: rule.into(),
            at_least,
            target: service.target,
            met,
            budget_seconds: service
                .target
                .filter(|_| measured.counted > 0.0)
                .map(|t| error_budget(t, measured.counted, measured.down)),
            members,
            numbers: Numbers::of(&measured),
            outage_list: b.outages(&outages),
        });
    }
    views
}

/// The silences that covered something of the report, within the period.
fn maintenance(b: &Builder) -> Vec<MaintenanceView> {
    let end = b.period.1.min(b.now);
    let mut views = Vec::new();
    for silence in &b.data.silences {
        let start = silence.span.0.max(b.period.0);
        let stop = silence.span.1.min(end);
        if start >= stop {
            continue;
        }
        let servers: Vec<String> = b
            .data
            .machines
            .iter()
            .filter(|m| {
                whole(silence, m)
                    || names_checks_of(silence, m).is_some()
                    || silence.relay_id.is_some_and(|r| m.relays.contains(&r))
            })
            .map(|m| m.name.clone())
            .collect();
        if servers.is_empty() {
            continue;
        }
        views.push(MaintenanceView {
            start,
            end: stop,
            reason: silence.reason.clone(),
            servers,
            checks: silence.checks.clone().unwrap_or_default(),
            window: silence.window,
        });
    }
    views
}

#[cfg(test)]
mod tests;
