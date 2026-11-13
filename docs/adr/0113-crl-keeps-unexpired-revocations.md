# ADR-0113: The CRL lists only certificates that have not expired

Date: 2026-11-12
Status: accepted; amends ADR-0007, ADR-0025 and ADR-0088

## Context

A security audit (2026-11-12) found that the CRL only grows. `revocations.json` kept every revoked serial forever and each CRL listed all of them, about 35 bytes each. `GetCrlResponse` carried the active CA's CRL twice (`crl_der` and `crl_ders[0]`, ADR-0088), three times' worth during a change of CA, and relays decoded it with tonic's default 4 MiB limit.

So at about 60,000 revocations (40,000 during a change) every relay failed to fetch the CRL, and 24 hours later, when its last CRL expired, refused all its agents. A customer administrator could get there alone by enrolling hosts with an install key and removing them. The way back was editing `revocations.json` by hand.

## Decision

**A revocation records when its certificate expires.** `CertificateAuthority::revoke` takes the certificate's `not_after`, which every caller has: the revoke endpoints and the pending-host rejection read it from the `certificates` row they mark revoked, and the revocation sync between instances (ADR-0089) selects it with the row.

**Expired certificates leave the CRL and the file.** Each CRL is built from the revocations whose `not_after`, plus a margin of one day (`CaOptions::revocation_margin`), is not past; the others are dropped from `revocations.json` in the same write that takes the CRL number. Every verifier checks expiry before the CRL, so an expired serial in the CRL protected nothing. The margin covers relays and other instances whose clock runs late.

**Older records** have no `not_after`. The CA now refuses to issue a certificate valid for more than 90 days (`CaOptions::max_cert_validity`, the longest it ever issued: the server's own certificate and the stores' certificates), and a certificate is revoked after it is issued, so such a record is kept until 90 days, plus the margin, after its `revoked_at`. No database lookup at start, and the CA keeps working without the database.

**The sync does not bring them back.** It already selects only `not_after > now()`, so a serial the CA forgot is not revoked again.

**Each CRL travels once.** `GetCrlRequest.reads_crl_ders` (new, additive) says the caller reads `crl_ders`; the server and relays then leave `crl_der` empty. Relays of this version set it. Older callers do not and get both fields as before: relays older than ADR-0088 read only `crl_der`, and relays between ADR-0088 and this one read `crl_ders` when it is not empty, so neither changes. A relay of this version still falls back to `crl_der` when an older upstream sends no list.

**Relays accept up to 64 MiB** (`tls::MAX_CRL_RESPONSE_BYTES`) for `GetCrl`, instead of 4 MiB.

## Consequences

- The CRL holds the revocations of the last certificate lifetime: about 7 days for agents and relays, 90 days for the server and store certificates, which are rarely revoked. Hand edits are no longer the way out of a large CRL; time is.
- Enrolling and removing hosts in a loop can still grow the CRL within a week, but a 64 MiB response is about 950,000 revocations during a CA change, against the install key limit of 30 enrollments a minute per key, roughly 300,000 a week.
- `is_revoked` answers false for a serial once it is forgotten. Renewal and the stream check expiry first, so nothing that was refused becomes accepted.
- A caller cannot ask for more than 90 days of validity any more; none did.
