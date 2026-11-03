# ADR-0088: Changing the certificate authority

Date: 2026-11-03
Status: accepted; amends ADR-0007 and ADR-0010

## Context

The central server's built-in CA (ADR-0004, ADR-0007) signs the certificate of every agent and relay, the server's own, and every command (ADR-0031). Its certificate lasts 10 years, and nothing could replace it: a new CA meant installing every agent and relay again, by hand. That is the case at the end of its life, after a suspected leak of its key, and for anyone who wants to change keys on a schedule.

Peers stored exactly one CA. Renewal refused a response whose CA was not the stored one (ADR-0010), agents loaded the CA for command signatures once at start, relays enforced exactly one CRL, and the server's listeners were built once at start. Production hardening (Phase 12) needs the CA to change while every agent and relay keeps working, without touching any machine.

## Decision

### Two CAs at a time, three steps

During a change the server holds two CA generations. `<data_dir>/ca/ca-key.pem` and `ca-cert.pem` stay the **active** one, which issues certificates and signs commands, so a server that never changes keeps the layout of ADR-0004. A change adds `rotation.json`, the other generation in `next/` or `previous/`, and `retired/<sha256>.json` for each retired one (its certificate, never its key). Every step writes its files in an order that the next start finishes or undoes after a crash.

1. **Introduced.** A new CA is created and trusted next to the old one; the old one still issues. Every certificate the server issues now comes with both CAs. Done when every agent and relay with a valid certificate trusts the new CA.
2. **Switched.** The new CA becomes active: it issues, signs commands and CRLs, and the server's own certificate is issued again from it. Both CAs stay trusted. Every agent and relay gets a certificate from the new CA. Done when all have one.
3. **Retired.** The old CA is no longer trusted, its key is deleted, and peers drop it at their next check.

The server takes each step by itself (`ca_rotation::Rotator`, every 5 minutes) and reloads its listeners in place: the gRPC listener and the enrollment listener now follow a watch channel, as the gRPC listener already did for the CRL. No restart.

**How long.** Peers ask every hour (below), so with a connected fleet each step takes about an hour and a planned change about two hours. A peer that is offline is waited for until its certificate expires, at most 7 days per step: by then it could not connect anyway and needs a new install command, change or not. So a planned change takes two hours to two weeks. An administrator can take the next step at once ("Switch now", "Stop trusting the old authority now"); the machines not done yet then need a new install command, which the page says before the button is pressed.

**How the server knows.** `certificates` gets two columns (migration `20261114090000`): `issuer_ca`, the SHA-256 of the CA that signed it, and `trusted_cas`, the CAs the peer was given with it, or NULL for a peer too old to take more than one. A peer counts by its newest certificate that is neither expired nor revoked. Older rows get their issuer at start, from the only CA there was. The page then says "312 of 315 servers and relays moved to the new certificate authority; 3 have not connected since …", lists the 3 with their last contact and when they stop being waited for, and marks those too old to follow.

### Peers learn the new CA only over mTLS

- `RenewRequest.accepts_ca_bundle` and `RenewResponse.ca_bundle_pem`: a peer of this version says it stores a bundle; the response then carries every CA it must trust, the issuer first. `ca_cert_pem` is still the issuer alone, so older peers see what they always saw.
- `RenewRequest.rotation_only`: every hourly check of the renewal task (ADR-0010) also asks whether a change needs a new certificate here. The server compares the presented certificate's row with its CAs and answers `FAILED_PRECONDITION` when nothing changed, at trace level. When something did, it issues whatever the renewal interval says: the server decides when steps happen, and each step needs one certificate per peer. Asked through a relay, the fields travel in `ForwardedRenewRequest`.
- The peer accepts a bundle only if it starts with the CA that signed the new certificate, holds at most two CAs and keeps at least one it already trusts; the certificate must verify against its issuer. The answer arrived on a connection the stored CAs authenticated, so a peer only ever moves along a chain of CAs it trusted. The bundle is written to `ca.pem` before the key and certificate: it holds the CA of both the old and the new certificate, so a crash between the files leaves a working pair.
- Enrollment carries the same `accepts_ca_bundle` and `ca_bundle_pem`; the bundle must start with the pinned CA. First contact still only trusts the token's pin (ADR-0007).
- Nothing new is ever learned on first contact, from the enrollment listener, from port 80 or from a file.

Agents and relays verify command signatures against the CAs of their current identity instead of those loaded at start. A relay fetches the CRLs again whenever its certificate changes, so it has the new CA's CRL before the first agent presents a certificate from it.

### Emergency change

After a suspected leak, "Emergency change" skips the introduction: the new CA issues at once and the server presents a certificate from it. The old CA stays trusted for 24 hours (`distrust_at`), so that connected peers, which ask every hour, can move; it is retired then, or as soon as every peer moved. Tokens and install commands pinned to the old CA stop working at once.

A peer that misses the window has a certificate nobody trusts. The page lists it under "Needs a new install", from the same `certificates` rows (`cut_off`), and its offline incident shows on its own page. Installing it again is a new install on that machine: with a single-use token for its name it comes back as the same server, with its history; an install command gives it a new name (ADR-0073).

The old key stays on the server for those 24 hours, to sign the old CA's CRL: a CRL is only read by the server's own listener and by relays over mTLS, so a forged one buys an attacker nothing. A leaked key can still sign a certificate for any name during the window, but renewal and the stream only accept serials recorded in `certificates` (ADR-0010), so it cannot become a certificate of the new CA.

Peers older than this version cannot take a bundle. They block a planned switch until they update, which agents and relays do by themselves (ADR-0053, ADR-0055) with commands the old CA still signs during the introduction. An emergency does not wait for them.

### The CA's own end

The page shows when the CA was created and expires. A year before expiry it says so in plain words, with a button that starts a planned change. Six months before expiry, if nobody did, the server starts one by itself; within 30 days of the old CA's end, a planned switch stops waiting for the last peers. The server certificate never outlives its CA any more: it used to be refused 90 days before. Every start, step and button is in the audit log: `ca.rotation.start`, `ca.rotation.switch`, `ca.rotation.retire`, by the administrator or by `system`.

### Interactions

- **CRL and revocation.** One revocation list is shared, and each trusted CA signs a CRL of it with the same number: serials are 128 random bits, so a serial in the other CA's CRL revokes nothing else. The server's listener enforces both; `GetCrlResponse.crl_ders` gives relays both, and `crl_der` stays the active one's for older relays.
- **Browser certificate (ADR-0068).** It comes from Caddy's own CA, which this change does not touch.
- **Backups and restore (ADR-0058).** `rotation.json`, `next/` and `previous/` are in `serverdata` and restore with it, so a backup taken during a change resumes it. The restore keeps `ca/retired/` from the data it replaces. A backup from before a change brings back the old CA: the restore says so, and the server logs it and shows it on the page. The machines that moved cannot connect to it; a newer backup or new install commands are the way out. A restore on a fresh machine has no earlier data to compare with.
- **Relays offline during the switch** are waited for like agents, and their agents with them: an agent behind an offline relay cannot renew, so it is still waiting when the relay returns, and moves then.
- **Windows agents** keep their CAs in their own `ca.pem` under `C:\ProgramData\MonitorFlow\agent\tls`, written with the same ACL as before, not in the Windows certificate store. Nothing differs from Linux.
- **Install commands.** Tokens and install keys pin the CA that was active when they were made (ADR-0007, ADR-0073). Peers check the pin against the certificate the enrollment listener presents, so commands made before the switch stop working after it, and the page says so. New ones pin the new CA.
- **Other certificates from the CA.** Anything else the server issues from its CA follows the same rule as its own certificate: issue it again from the active CA when the one it has comes from another.

## Alternatives rejected

- **A new message on the stream telling peers to renew.** Faster by up to an hour, but relays forward envelopes without reading them (ADR-0001), so it would need code in the agent's stream, the relay's own stream and the server's registry. The hourly question goes through the renewal path relays already forward.
- **Cross-signing the new CA with the old one.** It would let peers that never learned the new CA verify the server, and keep old install commands working through the switch. It adds certificates whose life outlasts the change, and a peer that never learned the new CA still has to before the old one is retired. A possible follow-up for install commands only.
- **Moving every peer at once, without the introduction.** A peer that did not pick up the new CA in time could not authenticate the server any more. The emergency change accepts that risk; a planned one has no reason to.
- **Keeping the change's state in the database.** It must stay next to the keys: a backup restores both together, and the server must be able to load its CA before the database.
- **A setting for the step durations.** The rules above end each step as soon as it can and never later than the certificates allow.

## Consequences

- An administrator changes the CA in one click, and the server can do it by itself before it expires; no machine needs a visit unless it stays offline for more than a week or misses an emergency.
- Every peer makes one more unary call an hour, with a fresh key and CSR: about 0.3 calls a second for 1,000 agents, almost all answered without touching the CA.
- Install commands made before a switch must be copied again.
- High availability must share `ca/` between servers and make the others reload when one takes a step. Today one server drives the change.

Not done yet, for later:

- Cross-signing for the enrollment listener, so that install commands survive a planned switch.
- A notification when a change starts by itself or an emergency window is about to close, besides the page and the audit log.
- A test with a real Windows agent across a change; the code path is the one Linux uses.
