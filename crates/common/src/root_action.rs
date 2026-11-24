//! AI root actions (ADR-0181): the fixed catalog of root operations the
//! AI may ask for, instead of a command line. The AI writes one as
//! `monitorflow-root <action> [argument]`; the server's risk rules read it
//! with [`RootAction::parse`], send it as a signed `root_action` command
//! ([`RootAction::to_proto`]), and the agent's root helper reads it back
//! with [`RootAction::from_proto`], which checks every parameter again.
//! Nothing else reaches root: no option, no path, no free text.

use std::fmt;

use crate::proto::v1::{self, root_action::Action};

/// The program name the AI writes, which no host has: the server routes
/// it to the agent's root helper.
pub const PROGRAM: &str = "monitorflow-root";

/// Smallest size the journal may be vacuumed to, in megabytes: the
/// recent logs stay.
pub const MIN_JOURNAL_MB: u32 = 100;
/// Largest size accepted, in megabytes (64 GiB).
pub const MAX_JOURNAL_MB: u32 = 64 * 1024;

/// Longest unit name, as systemd allows.
const MAX_UNIT: usize = 255;

/// Services never restarted by a root action: losing one cuts the host off
/// (ssh, network, D-Bus), or stops MonitorFlow itself mid-command.
const CRITICAL_SERVICES: &[&str] = &[
    "ssh",
    "sshd",
    "dbus",
    "dbus-broker",
    "networking",
    "network",
    "NetworkManager",
    "polkit",
    "init",
    "monitorflow-agent",
    "monitorflow-relay",
    "monitorflow-server",
];

/// Unit types other than services, refused by their suffix.
const OTHER_UNIT_TYPES: &[&str] = &[
    "socket",
    "target",
    "mount",
    "automount",
    "swap",
    "path",
    "timer",
    "slice",
    "scope",
    "device",
];

/// Prefixes of the same kind: systemd's own services and the consoles.
const CRITICAL_PREFIXES: &[&str] = &[
    "systemd-",
    "getty",
    "serial-getty",
    "user@",
    "user-runtime-dir@",
];

/// One action of the catalog, with its parameters checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootAction {
    /// `clean-package-cache`: the package manager's download cache.
    CleanPackageCache,
    /// `vacuum-journal <size>`: the systemd journal down to `max_megabytes`.
    VacuumJournal { max_megabytes: u32 },
    /// `prune-docker`: unused Docker images and the build cache.
    PruneDocker,
    /// `restart-service <unit>`: one systemd service, `.service` added
    /// when the name has no suffix.
    RestartService { unit: String },
}

impl RootAction {
    /// The action named by `args`, the words after [`PROGRAM`]. `Err`
    /// says what is wrong in one sentence, with the expected syntax.
    pub fn parse(args: &[&str]) -> Result<Self, String> {
        let (name, rest) = args
            .split_first()
            .ok_or_else(|| format!("`{PROGRAM}` needs an action: {}", syntax()))?;
        let action = match (*name, rest) {
            ("clean-package-cache", []) => Self::CleanPackageCache,
            ("prune-docker", []) => Self::PruneDocker,
            ("vacuum-journal", [size]) => Self::VacuumJournal {
                max_megabytes: journal_size(size)?,
            },
            ("restart-service", [unit]) => Self::RestartService {
                unit: service(unit)?,
            },
            ("clean-package-cache" | "prune-docker" | "vacuum-journal" | "restart-service", _) => {
                return Err(format!(
                    "`{PROGRAM} {name}` takes other arguments: {}",
                    syntax()
                ))
            }
            _ => return Err(format!("`{name}` is not a root action: {}", syntax())),
        };
        Ok(action)
    }

    /// The stable name, as written after [`PROGRAM`] and in rule IDs.
    pub fn name(&self) -> &'static str {
        match self {
            Self::CleanPackageCache => "clean-package-cache",
            Self::VacuumJournal { .. } => "vacuum-journal",
            Self::PruneDocker => "prune-docker",
            Self::RestartService { .. } => "restart-service",
        }
    }

    pub fn to_proto(&self) -> v1::RootAction {
        let action = match self {
            Self::CleanPackageCache => Action::CleanPackageCache(v1::CleanPackageCache {}),
            Self::VacuumJournal { max_megabytes } => Action::VacuumJournal(v1::VacuumJournal {
                max_megabytes: *max_megabytes,
            }),
            Self::PruneDocker => Action::PruneDocker(v1::PruneDocker {}),
            Self::RestartService { unit } => {
                Action::RestartService(v1::RestartService { unit: unit.clone() })
            }
        };
        v1::RootAction {
            action: Some(action),
        }
    }

    /// The action of a signed payload, with the same checks as
    /// [`Self::parse`]: a payload the server should never have sent is
    /// refused all the same.
    pub fn from_proto(message: &v1::RootAction) -> Result<Self, String> {
        match &message.action {
            None => Err("the root action is empty".into()),
            Some(Action::CleanPackageCache(_)) => Ok(Self::CleanPackageCache),
            Some(Action::PruneDocker(_)) => Ok(Self::PruneDocker),
            Some(Action::VacuumJournal(v)) => {
                if (MIN_JOURNAL_MB..=MAX_JOURNAL_MB).contains(&v.max_megabytes) {
                    Ok(Self::VacuumJournal {
                        max_megabytes: v.max_megabytes,
                    })
                } else {
                    Err(size_range())
                }
            }
            Some(Action::RestartService(r)) => {
                let unit = service(&r.unit)?;
                if unit != r.unit {
                    return Err(format!("`{}` is not a full service name", r.unit));
                }
                Ok(Self::RestartService { unit })
            }
        }
    }
}

impl fmt::Display for RootAction {
    /// The canonical line, as the AI writes it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{PROGRAM} {}", self.name())?;
        match self {
            Self::VacuumJournal { max_megabytes } => write!(f, " {max_megabytes}M"),
            Self::RestartService { unit } => write!(f, " {unit}"),
            Self::CleanPackageCache | Self::PruneDocker => Ok(()),
        }
    }
}

fn syntax() -> String {
    format!(
        "`{PROGRAM} clean-package-cache`, `{PROGRAM} vacuum-journal <size>` (like 500M or 2G), \
         `{PROGRAM} prune-docker` or `{PROGRAM} restart-service <unit>`"
    )
}

fn size_range() -> String {
    format!(
        "the journal size must be between {MIN_JOURNAL_MB}M and {}G",
        MAX_JOURNAL_MB / 1024
    )
}

/// `500M` or `2G` in megabytes, within the bounds.
fn journal_size(text: &str) -> Result<u32, String> {
    let bad = || format!("`{text}` is not a size like 500M or 2G");
    let (digits, unit) = text.split_at(text.len().saturating_sub(1));
    let factor: u64 = match unit {
        "M" | "m" => 1,
        "G" | "g" => 1024,
        _ => return Err(bad()),
    };
    if digits.is_empty() || digits.len() > 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let megabytes = digits.parse::<u64>().map_err(|_| bad())? * factor;
    u32::try_from(megabytes)
        .ok()
        .filter(|mb| (MIN_JOURNAL_MB..=MAX_JOURNAL_MB).contains(mb))
        .ok_or_else(size_range)
}

/// `unit` as a full service name (`nginx` is `nginx.service`), refused
/// when it is not a plain name, not a service, or a critical service.
fn service(unit: &str) -> Result<String, String> {
    let plain = (1..=MAX_UNIT - ".service".len()).contains(&unit.len())
        && !unit.starts_with(['-', '.', '@'])
        && unit
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b":_.@-".contains(&b));
    if !plain {
        return Err(format!("`{unit}` is not a systemd unit name"));
    }
    let full = match unit.rsplit_once('.') {
        Some((_, "service")) => unit.to_owned(),
        Some((_, suffix)) if OTHER_UNIT_TYPES.contains(&suffix) => {
            return Err(format!(
                "only services can be restarted, not a .{suffix} unit"
            ))
        }
        // Dots also appear in plain names (`php8.2-fpm`, `openvpn@site.a`).
        _ => format!("{unit}.service"),
    };
    let base = full.strip_suffix(".service").unwrap_or(&full);
    if base.ends_with('@') {
        return Err(format!("`{unit}` is a template; name one instance of it"));
    }
    let name = base.split('@').next().unwrap_or(base);
    if CRITICAL_SERVICES.contains(&name) || CRITICAL_PREFIXES.iter().any(|p| base.starts_with(p)) {
        return Err(format!(
            "`{full}` is critical to reaching or monitoring the host and is never restarted this way"
        ));
    }
    Ok(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Result<RootAction, String> {
        RootAction::parse(&line.split_whitespace().collect::<Vec<_>>())
    }

    #[test]
    fn reads_every_action_of_the_catalog() {
        assert_eq!(
            parse("clean-package-cache"),
            Ok(RootAction::CleanPackageCache)
        );
        assert_eq!(parse("prune-docker"), Ok(RootAction::PruneDocker));
        assert_eq!(
            parse("vacuum-journal 500M"),
            Ok(RootAction::VacuumJournal { max_megabytes: 500 })
        );
        assert_eq!(
            parse("vacuum-journal 2g"),
            Ok(RootAction::VacuumJournal {
                max_megabytes: 2048
            })
        );
        assert_eq!(
            parse("restart-service nginx"),
            Ok(RootAction::RestartService {
                unit: "nginx.service".into()
            })
        );
        assert_eq!(
            parse("restart-service php8.2-fpm.service"),
            Ok(RootAction::RestartService {
                unit: "php8.2-fpm.service".into()
            })
        );
        assert_eq!(
            parse("restart-service php8.2-fpm"),
            Ok(RootAction::RestartService {
                unit: "php8.2-fpm.service".into()
            })
        );
        assert_eq!(
            parse("restart-service openvpn@site.a"),
            Ok(RootAction::RestartService {
                unit: "openvpn@site.a.service".into()
            })
        );
    }

    #[test]
    fn refuses_anything_else() {
        for line in [
            "",
            "apt-get clean",
            "clean-package-cache --all",
            "clean-package-cache now",
            "prune-docker --volumes",
            "vacuum-journal",
            "vacuum-journal 500",
            "vacuum-journal 99M",
            "vacuum-journal 65G",
            "vacuum-journal 0G",
            "vacuum-journal -5M",
            "vacuum-journal 500M 1G",
            "vacuum-journal 9999999M",
            "vacuum-journal 1T",
            "restart-service",
            "restart-service a b",
            "restart-service sshd",
            "restart-service ssh.service",
            "restart-service systemd-journald",
            "restart-service getty@tty1",
            "restart-service user@1000.service",
            "restart-service monitorflow-agent",
            "restart-service NetworkManager",
            "restart-service multi-user.target",
            "restart-service var.mount",
            "restart-service docker.socket",
            "restart-service apt-daily.timer",
            "restart-service -nginx",
            "restart-service ../nginx",
            "restart-service nginx;reboot",
            "restart-service ngi\\x2dnx",
            "restart-service foo@",
            "restart-service foo@.service",
            "restart-service --force",
        ] {
            assert!(parse(line).is_err(), "{line}");
        }
        let long = format!("restart-service {}", "a".repeat(250));
        assert!(parse(&long).is_err());
    }

    #[test]
    fn errors_give_the_syntax() {
        let error = parse("rm -rf /").unwrap_err();
        assert!(
            error.contains("monitorflow-root vacuum-journal <size>"),
            "{error}"
        );
        let error = parse("vacuum-journal 10M").unwrap_err();
        assert!(error.contains("between 100M and 64G"), "{error}");
    }

    #[test]
    fn the_payload_round_trips_and_is_checked_again() {
        for line in [
            "clean-package-cache",
            "prune-docker",
            "vacuum-journal 1G",
            "restart-service nginx",
        ] {
            let action = parse(line).unwrap();
            assert_eq!(
                RootAction::from_proto(&action.to_proto()),
                Ok(action.clone())
            );
            assert_eq!(
                parse(&action.to_string()["monitorflow-root ".len()..]),
                Ok(action)
            );
        }
        let bad = |action| v1::RootAction {
            action: Some(action),
        };
        assert!(RootAction::from_proto(&v1::RootAction::default()).is_err());
        for message in [
            bad(Action::VacuumJournal(v1::VacuumJournal {
                max_megabytes: 1,
            })),
            bad(Action::VacuumJournal(v1::VacuumJournal {
                max_megabytes: u32::MAX,
            })),
            bad(Action::RestartService(v1::RestartService {
                unit: "sshd.service".into(),
            })),
            // The helper never completes a name: the server sends it whole.
            bad(Action::RestartService(v1::RestartService {
                unit: "nginx".into(),
            })),
            bad(Action::RestartService(v1::RestartService {
                unit: "nginx.service --now".into(),
            })),
        ] {
            assert!(RootAction::from_proto(&message).is_err(), "{message:?}");
        }
    }
}
