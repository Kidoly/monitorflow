//! Which snapshot a server gets before a change (ADR-0094), in one order
//! for the remediation hook, the approval queue, the server page and
//! campaigns:
//!
//! 1. A Proxmox mapping, by hand or by discovery (ADR-0093), while
//!    `[proxmox]` is on: Proxmox snapshots the VM or container. When it
//!    cannot (storage that cannot snapshot, a cluster that no longer
//!    serves the server), rollback is limited for Proxmox's reason: a
//!    guest is never also snapshotted from inside, which would put two
//!    rollbacks on one machine, and the fix is on the hypervisor.
//! 2. Otherwise a local LVM or ZFS snapshot taken by the agent, when the
//!    server's system volumes allow one.
//! 3. Otherwise rollback is limited, for the local reason.
//!
//! A limited rollback never stops a change: the approver saw it before
//! approving (ADR-0093).

use std::fmt;

use super::local::{LocalPlan, LocalUnavailable};
use super::{Support, Unsupported, VmRef};

/// What a change on a server gets.
#[derive(Debug, Clone, PartialEq)]
pub enum Choice {
    Proxmox(VmRef),
    Local(LocalPlan),
    Limited(Limited),
}

/// Why a change gets no snapshot.
#[derive(Debug, Clone, PartialEq)]
pub enum Limited {
    /// The server is a mapped guest Proxmox cannot snapshot now.
    Proxmox(Unsupported),
    /// The server is not a Proxmox guest and has no local snapshot.
    Local(LocalUnavailable),
}

impl fmt::Display for Limited {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Proxmox(reason) => reason.fmt(f),
            Self::Local(reason) => reason.fmt(f),
        }
    }
}

/// Whether Proxmox's answer `reason` settles it: the server is a guest
/// mapped while the integration is on, so no local snapshot is tried.
pub fn proxmox_decides(reason: Unsupported) -> bool {
    match reason {
        Unsupported::Disabled | Unsupported::NotMapped => false,
        Unsupported::UnknownCluster | Unsupported::CannotSnapshot(_) => true,
    }
}

/// The choice from Proxmox's `support` and, only when Proxmox has no say,
/// the local plan.
pub fn choose(
    proxmox: Support,
    local: impl FnOnce() -> Result<LocalPlan, LocalUnavailable>,
) -> Choice {
    match proxmox {
        Support::Available(vm) => Choice::Proxmox(vm),
        Support::Unavailable(reason) if proxmox_decides(reason) => {
            Choice::Limited(Limited::Proxmox(reason))
        }
        Support::Unavailable(_) => match local() {
            Ok(plan) => Choice::Local(plan),
            Err(why) => Choice::Limited(Limited::Local(why)),
        },
    }
}

#[cfg(test)]
mod tests {
    use monitorflow_common::snapshot::Refusal;

    use super::*;
    use crate::remediation::snapshot::GuestKind;

    fn vm() -> VmRef {
        VmRef {
            cluster: "pve".into(),
            node: "pve1".into(),
            vmid: 101,
            kind: GuestKind::Qemu,
        }
    }

    fn plan() -> LocalPlan {
        LocalPlan {
            method: "lvm",
            covered: vec!["/".into()],
            not_covered: Vec::new(),
            summary: "LVM snapshot of / (the thin pool has 40.0 GiB free)".into(),
        }
    }

    fn plain() -> LocalUnavailable {
        LocalUnavailable::Refused(Refusal::RootNotCovered {
            fs_type: "ext4".into(),
        })
    }

    #[test]
    fn a_proxmox_guest_never_gets_a_local_snapshot() {
        let asked = std::cell::Cell::new(false);
        let local = || {
            asked.set(true);
            Ok(plan())
        };
        assert_eq!(
            choose(Support::Available(vm()), local),
            Choice::Proxmox(vm())
        );
        assert!(!asked.get(), "the local plan is not even read");
        for reason in [
            Unsupported::CannotSnapshot(GuestKind::Lxc),
            Unsupported::UnknownCluster,
        ] {
            assert_eq!(
                choose(Support::Unavailable(reason), || Ok(plan())),
                Choice::Limited(Limited::Proxmox(reason))
            );
        }
    }

    #[test]
    fn otherwise_local_then_limited() {
        for reason in [Unsupported::Disabled, Unsupported::NotMapped] {
            assert_eq!(
                choose(Support::Unavailable(reason), || Ok(plan())),
                Choice::Local(plan())
            );
            let limited = choose(Support::Unavailable(reason), || Err(plain()));
            assert_eq!(limited, Choice::Limited(Limited::Local(plain())));
            let Choice::Limited(why) = limited else {
                unreachable!()
            };
            assert_eq!(why.to_string(), "/ is on plain ext4");
        }
    }
}
