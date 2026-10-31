# ADR-0063: Agent on any Linux

Date: 2026-10-31
Status: accepted

## Context

The agent refused to start outside the Debian and RHEL families: an Alpine, Arch or Fedora host got "unsupported distribution" and could not be monitored at all, though every sensor but the package report works there (simplicity audit, M5). A host of a known family could still lack its tools, as on image-based systems, and then failed its package report every few minutes. The binary is static, so the distribution only matters to package features.

## Decision

**The agent starts on any Linux.** Reading `os-release` no longer fails: an unknown `ID`, a missing or unreadable file all give the new family `linux`, and the version is whatever the file says, possibly nothing. This does not widen the families: Fedora, Amazon Linux and the like are `linux` too, as apt and dnf command lines are tested against the families only.

**The capability comes from the agent.** At start it checks that its family's tools exist (`apt-get` and `dpkg-query`, or `dnf` and `rpm`) and says so in the new `Hello.no_package_manager`. Without them it sends no package reports and refuses `apply_updates`, `configure_sources` and `revert_sources` as unsupported, like Windows does for what it lacks (ADR-0047). The flag is negative so that an agent that does not send it means what it always did: older agents refused any host outside the families. A tool installed later counts at the next agent start.

**The server stores it** as `servers.package_updates`, updated by each `Hello`, and returns it in the server views. Family `linux` alone also makes it false, in case an older relay dropped the flag. The order of ADR-0055 is kept: a server this new accepts every older relay and agent. An older server refuses family `linux`, and only that: a known family's agent keeps connecting.

**Hosts without package updates are left out, not failed.**

- The API answers 409 to `apply_updates` and to source commands for them.
- A package campaign leaves them out of its targets when created, whether named or through the site; the form shows them disabled and counts them. A target that loses the capability after creation leaves the campaign at its wave, so it does not count towards the failure threshold.
- Agent updates and every other feature do not depend on it.
- The server page says "Package updates aren't available on this system; monitoring works normally", and its Updates tab explains why.

**The installer needs systemd, not a distribution.** It never checked the distribution; it checks what it cannot work without, before changing anything, and names it: systemd, the tools it runs, and writable `/usr/bin` and `/etc/systemd/system`. Removing an agent no longer requires systemd. It does not write unit files for other init systems: the first start, the enrollment wait and the restart of self-updates all rely on systemd, and a second implementation of each is a lot to carry for hosts that can run the binary by hand. The self-update helper fails on them as it does on any failed watchdog start: it puts the previous binary back and reports the error.

## Consequences

- Monitoring reaches every Linux host with a supported architecture; the `systemd` sensor fails on hosts without systemd like any failing sensor, the rest are unaffected.
- `os_family` has a fourth value, which needs a new check constraint (`20261105110000`).
- A host that loses apt keeps its last package report in the database, marked by its age.
- Hosts without systemd have no one-line install and no self-update. If they become common, an OpenRC path is the next step, with its own ADR.
- A family's new member needs no change on the server: adding an `ID` to the agent's table moves the host from `linux` to that family at its next start.
