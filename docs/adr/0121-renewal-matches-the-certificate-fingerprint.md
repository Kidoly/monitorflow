# ADR-0121: Renewal matches the certificate's fingerprint, not only its serial

Date: 2026-11-12
Status: accepted

## Context

`Renew` and `RenewForwarded` (ADR-0010, ADR-0028) find the presented certificate in `certificates` by serial, then check the role and ID. The TLS handshake (or `verify_issued` for a forwarded certificate) proves a trusted CA signed it, but not that it is the certificate this server issued with that serial. During an emergency change of certificate authority (ADR-0088), the old, possibly compromised CA stays trusted for 24 hours. Serials are not secret: they appear in audit details, logs and API answers. Whoever holds the old CA key can mint a certificate with the identity and serial of a real, valid one, connect and renew. The server then hands them a certificate from the new CA, and their access outlives the distrust of the old CA. The rate limit does not apply, because the certificate is behind on the CA change.

## Decision

**Record the certificate's SHA-256 and require it.** A new nullable column `certificates.cert_sha256` holds the lowercase hexadecimal SHA-256 of each issued certificate's DER, as `issuer_ca` does for CAs. `renew::record_certificate` fills it, for enrollment and renewal alike. `issue_renewal`, which serves direct and forwarded renewals, refuses with `PERMISSION_DENIED` when the presented certificate's SHA-256 is not the one recorded with its serial. A copy with a real serial but another key, or any other byte changed, no longer renews.

**Rows from before the column.** They have no fingerprint and the server cannot compute one: it never kept the DER. They renew on their serial, as before, except while an emergency change is under way (`Trust::emergency`), when the refusal tells the peer to enroll again. Certificates last 7 days by default and peers renew after two thirds of that, so such rows stop being used within a week of the upgrade; an emergency change in that week costs a re-enrollment for the peers that did not renew yet, which is the price of not letting a distrusted CA mint its way into the new one. Outside an emergency, every CA that signs is one the server trusts on its own, so a serial-only match there gives nothing a forger did not already have.

## Alternatives rejected

- **Store the whole DER.** The hash is enough to compare, and smaller.
- **Refuse rows without a fingerprint at once.** Every peer would need a new token after the upgrade.
- **Fill the fingerprint of an old row from the first certificate that renews with it.** The first caller could be the forger.

## Consequences

- One more column per certificate row, filled for every new row.
- Peers still on a pre-upgrade certificate during an emergency change enroll again.
- Rows inserted by hand (tests, `scripts/dev-certs.sh` certificates, which were never recorded anyway) behave as old rows.
