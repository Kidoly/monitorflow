# ADR-0070: Plain words and the certificate warning

Date: 2026-11-01
Status: accepted; amends ADR-0020

## Context

ADR-0020 shows a certificate as a warning under 14 days (`CERT_WARNING_DAYS`). Peers' certificates last 7 days and renew once two thirds have passed (ADR-0010), so every healthy server carried a warning from its first second, and a new user tried to fix it (simplicity audit, M4). The same audit found jargon on the first screens: peer ID, revision, hysteresis, enrollment (M9).

## Decision

**The warning is about renewal, not about expiry.** Renewal is late under 2 days left (`RENEWAL_LATE_DAYS`), which a healthy peer never reaches, as it renews with about 2.3 days left and retries every 30 minutes at most. It has failed once the certificate has expired. Otherwise the screens say the certificate is valid and renews by itself, with the date under Details.

**"Failed" is read from the expiry date.** Peers do not report failed renewals, and adding that to the protocol for one label is not worth it: a certificate that expired is one that did not renew, whatever the reason. A renewal failing for hours shows nothing until 2 days are left, and agents log it.

**Words differ from names on purpose.** `docs/glossary.md` holds the words for screens, installers, CLI output and guides. API routes, configuration keys, database columns, protocol names, CLI flags and gRPC status messages keep their names, so nothing existing breaks. "Server" now means a machine with the agent everywhere, "host" goes, and "central server" names the machine running MonitorFlow.

## Consequences

- A new server shows no warning, and a warning means something needs doing.
- With a certificate validity above 7 days, renewal would start earlier and the 2-day mark would still mean late. Below about 6 days it would warn on healthy peers; the validity is not configurable today, so a change there must revisit `RENEWAL_LATE_DAYS`.
- Words and names now differ, so code and screens need the glossary to be read together.
