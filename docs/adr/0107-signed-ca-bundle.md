# ADR-0107: A new certificate authority must be signed for by a trusted one

Date: 2026-11-10
Status: accepted; amends ADR-0010 and ADR-0088

## Context

A security audit (2026-11-09) found that a compromised relay could give the peers below it a certificate authority of its own, and then sign commands for them.

A relay terminates its agents' mTLS, so it answers their `Renew` (ADR-0028), including the hourly question of ADR-0088. A peer accepted a CA bundle that started with the CA of the new certificate, held at most two CAs and kept one it already trusted. A relay could therefore answer with a certificate for the peer's key and name signed by its own CA, and the bundle `[its CA, the real CA]`. The peer stored it, and commands are verified against the CAs of the stored identity (ADR-0031). ADR-0088 argued that "the answer arrived on a connection the stored CAs authenticated", which is true of the relay's certificate and says nothing about who chose the answer.

A relay must stay unable to inject commands (architecture §4). Blocking or delaying its peers is within what a relay can already do.

## Decision

**The central server signs every bundle it sends with a renewal, and a peer takes a CA it does not trust yet only with such a signature from a CA it already trusts.**

**What is signed.** `"monitorflow-ca-bundle-v1\0"`, the SHA-256 of the new certificate, the number of CAs as 4 bytes, and the SHA-256 of each CA in the bundle's order, issuer first (`common::ca_bundle`). The domain differs from those of commands and plugin grants, and from certificates and CRLs, which are DER and start with 0x30, so a signature of one never passes for another.

**No replay.** The certificate holds the key its peer generated for that one call, with the peer's identity and a serial. The signatures are of use only to the holder of that private key, for that one answer: an older bundle that names a retired CA is bound to a certificate whose key the peer no longer has. This needs no clock on the peer and no counter on either side.

**Which keys.** The server signs once with the key of each CA it trusts: one signature normally, two during a change (`CertificateAuthority::sign_ca_bundle`). A peer that holds only the old CA, because it missed the introduction or because the change is an emergency, verifies the old CA's signature; one that holds only the new CA verifies the new one's. The old key exists until the old CA is retired, emergency window included (ADR-0088). `RenewResponse.ca_bundle_signatures` (field 6) carries them, for `Renew` and `RenewForwarded` alike; a relay passes the response on as it is.

**The peer's rule**, after the checks of ADR-0010 and ADR-0088:

- a bundle that adds no CA to the stored ones (the same, reordered, or fewer) needs no signature;
- a bundle that adds one needs a signature that verifies against a stored CA. Otherwise the whole answer is refused, nothing is written, and the peer asks again at its next hourly check.

Removing a CA never gives anyone trust, and refusing an unsigned removal would keep a retired CA, possibly a leaked one, trusted for longer; so removals stay unsigned. This also keeps renewals working against a server older than this decision as long as its CAs do not change.

**Enrollment is not signed.** The enrolling peer verifies the listener's certificate, by name, against the CA its token pins (ADR-0007); no relay is on that path, and the intended passthrough of ADR-0028 keeps TLS end to end. The peer has nothing but that pin to verify a signature with.

### What a relay can still do

Drop the signatures, or the whole answer. Its peers then do not learn a new CA and are cut off when the server switches to it: a denial of service, as dropping their streams is. It cannot make them trust anything.

### Versions

| Peer | Relay | Server | During a change of CA |
|---|---|---|---|
| older | any | this | The peer ignores the signatures and moves as before; it stays exposed to its relay until it updates. |
| this | this or none | this | Signed; planned and emergency changes work. |
| this | any | older | The peer refuses the new CA and stays where it is. |
| this | older | this | The older relay drops the field when it encodes the answer again (prost keeps no unknown field), so the peer refuses the new CA. |

Outside a change, every row renews normally. Automatic updates keep server ≥ relay ≥ agent (ADR-0055), so the last two rows need an agent installed by hand in a newer version than what is above it. The order for a real change is therefore: update the server, let relays and then agents follow, then start the change.

The server counts a peer as trusting the new CA from the certificate it issued (ADR-0088), not from what the peer stored, so a peer of the last row is counted as moved although it refused, and is cut off at the switch. A lost answer already had that effect for one peer. The follow-up below closes both.

### Emergency change

When the old key is the leaked one, the server still signs with it: it is the only key that peers which have not moved can verify. A relay without a CA key can plant nothing, at any step.

Whoever holds a CA key that a peer still trusts, and can answer that peer's renewal (a compromised relay, or a place on the network with a server certificate made with the key), can sign for another CA, and that CA outlives the retirement of the leaked one on that peer. The same holder could already sign commands for that peer during the window. No protocol closes this: the peer cannot tell the thief from the server. After a confirmed leak, machines the thief could reach during the window are installed again.

## Alternatives rejected

- **The signature as one more PEM block inside `ca_bundle_pem`.** Older relays would pass it on. But older peers would write it into `ca.pem`, and a protocol element would hide in a text field. The update order of ADR-0055 makes the dropped field rare.
- **A timestamp or a change counter in the signed bytes.** It needs peers' clocks, or state on both sides, to do what the fresh key of each renewal already does.
- **Cross-certificates**, the new CA signed by the old one. They are bound to no request, so one can be replayed for as long as it is valid; ADR-0088 already rejected them for other reasons.
- **Signing with the active key only.** After the switch, a peer that holds only the old CA, which is every peer in an emergency, could not verify it.
- **A signature for every bundle, or for removals.** See the peer's rule.
- **Accepting the certificate and leaving out the unsigned CA.** The peer would then present a certificate the server recorded with both CAs, and would never be offered the new CA again.

## Consequences

- A compromised relay can no longer sign commands for its subtree by way of a renewal; the "Signed commands" boundary of SECURITY.md holds again.
- Two more ECDSA signatures per renewal at most.
- A peer behind a relay or a server older than this decision cannot follow a change of CA until they update.

Not done yet, for later:

- Counting a peer as moved from the certificate it presents at its next hourly question, not from the one the server issued: a peer that refused or lost an answer then keeps the change waiting. Looking at versions alone (an agent newer than its relay) would stop counting too early, as soon as the relay updates and before the agent asks again.
