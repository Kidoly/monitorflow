# ADR-0066: Relay onboarding without typing

Date: 2026-10-31
Status: accepted

## Context

Adding a relay meant creating a token on the tokens page, typing an ID and the "server names" agents use to reach it, typing a mirror URL, then typing the relay's address again for each agent (simplicity audit, B9 and journey d). A novice does not know what to put in "server names", and the host knows its own names better than the administrator does.

ADR-0025 made one rule the basis of a relay's certificate: "the names come from the administrator, never from the CSR, so a relay cannot obtain a certificate for the server's name". Having the relay propose its names changes who proposes them. What must not change is who decides, because a relay's certificate is what agents verify it against:

- **Posing as the server.** With the server's name on it, a relay could accept the agents of every site.
- **Posing as another relay.** With the name of a relay of another site or customer, it could accept that relay's agents, if it can intercept their traffic.
- **Speaking for agents.** ADR-0058 kept install keys away from relays for a reason that reporting names does not touch: a relay certificate may forge samples for all the agents of its site (architecture §4), so a leaked reusable key would hand that out.
- **Wildcards and local names.** `*.example.com`, or `localhost`, would make one certificate match too much.

Administrators can also be careless, and an administrator of one customer must not be able to reach into another's agents, even on purpose.

## Decision

**Relays › Add relay.** The administrator picks a site and gets the relay's one-line command (ADR-0055) at the public address setting. No ID, no names. The command is the only thing to paste.

**An install key for one relay.** `install_keys` gets a `role`. A `relay` key has the format, storage, audit, revocation and limiter of ADR-0058, and takes the ID from the host name the same way, in the namespace of relays; a removed relay of the same name in the same site comes back (ADR-0062). The difference is its scope: one host, no approval, which a database constraint keeps so, and a day by default. Keys fit better than tokens because the ID must come from the host; a reusable key does not fit because relays are few and each one matters, so there is no use for it and the risk above.

**The relay reports, and nothing else follows from it.** `EnrollRequest.server_names` (field 4, additive) carries the relay's host name, if it is a full DNS name, then the addresses of its interfaces without loopback, link-local and container bridges, then a bare host name. The server treats them as untrusted: it bounds them, drops what could not be used, and stores them in `relays.pending_names` with the address the request came from. They are not written on the certificate: the CA issues a client certificate as for an agent, with no name and no `serverAuth`.

**A waiting relay is not yet a relay for anyone else.** While `pending_names` is set, the relay policy (`relay_may_speak_for`, `relay_route_allowed`) refuses it for every agent and child relay. It keeps its own stream, so it shows online and can self-update, and it can renew its certificate like any peer. It does not start its listener or its mirrors. Whoever holds a leaked key therefore gets an entry on the administrator's page, nothing more, and uses up the key, which the real installation will report as refused.

**An administrator decides.** `POST /api/relays/{id}/names`, for administrators of the relay's customer (ADR-0029), audited as `relay.confirm_names` with both the reported and the confirmed names, sets `relays.confirmed_names` and clears `pending_names`. The form shows the names ticked and the address the relay came from, and offers one field for a name the relay could not know, like the name of a NAT. A relay that should not be there is removed like any other (ADR-0062).

**What confirmation refuses, on the server, whatever the interface sends:**

- wildcards, `localhost` and `*.localhost`, loopback, link-local, multicast and unspecified addresses;
- whatever the server's certificate or public address covers, `*.` matching as a TLS client matches, so a relay cannot pose as the server, for any administrator;
- a DNS name or public address held by an unrevoked relay of another site, in any customer. The refusal does not say which relay. Relays of one site may share a name, as they may speak for the same agents;
- except that private addresses (RFC 1918, RFC 6598, unique local) may repeat across sites: `10.0.0.5` is a different host on each isolated network, and an MSP's sites overlap all the time. Reaching the agents of another site at that address takes being on that network already.

**The relay collects its certificate by asking.** While its certificate names nothing, it calls `Renew` every 10 s with `names_only` (additive in `RenewRequest` and `ForwardedRenewRequest`). The server answers `FAILED_PRECONDITION` and issues nothing unless an administrator confirmed names the presented certificate lacks; then it issues one with those names and `serverAuth`, past the interval that limits renewals (ADR-0010), which is meant for renewals of the same certificate. Every later renewal takes the names from `confirmed_names`, the ones in the database rather than those presented. The relay restarts its stream on the new certificate as for any renewal, then starts the listener and the mirrors. A polled, unary call follows the rule of architecture §3: authenticated as the peer, nothing on agent streams, nothing new for chained relays, which forward it. A server push would need a new envelope kind and a resend after every reconnect, to save a few seconds.

**A token with names works as before.** It ignores what the relay reports, and the relay is not waiting. A relay token without names, which used to give a relay that refused to start, now gives a waiting relay when the relay reports names; an older relay, which reports none, behaves as it did. Enrolling again with a token clears confirmed names, as the token's are the latest decision.

**Mirroring is a checkbox.** `MF_MIRROR=1` writes `[mirror] enabled = true`. With no `mirror.public_url`, the relay reports `http://<first name>:<mirror port>`, which is known once its names are confirmed. The interface drops its mirror URL field; `MF_MIRROR_URL` stays for scripts and for a URL that differs. Choosing what to mirror stays under Mirrors.

**"Add servers behind this relay".** The relay's page offers it once the relay has names: one click creates a key of the site with the defaults of ADR-0058 and shows the command with the relay's first name in "Connect through a relay", still editable. Confirming on the relays page links to it.

## Alternatives rejected

- **Issuing the names at enrollment and asking afterwards.** The certificate would already serve; revoking it after a wrong guess relies on every agent's CRL being fresh.
- **Holding the certificate until confirmation.** The installer waits for the identity files and gives up after two minutes, and the relay would not show online for the administrator to confirm.
- **Proving control of a name**, by the server connecting back to it. Relays exist for sites the server cannot reach, and a private name says nothing to the server.
- **A reusable relay key.** See Decision. Single-use tokens were the alternative, but they need a typed ID.
- **Refusing private addresses shared across sites.** It would refuse the normal case of an MSP.
- **A mirror URL field filled with a guess.** It is the question the audit wants gone.

## Consequences

- Adding a relay and the servers behind it takes one command, one confirmation and one command per host, without typing a name or looking up a guide.
- This needs a release whose relay sends its host name; an older relay gets the existing `INVALID_ARGUMENT` that says so. Relay self-update does not help a relay that is not installed yet, so Add relay is usable once a release ships this.
- `mirror.enabled = true` without `public_url` used to leave agents on the server's mirrors; the relay now reports its first name, and agents use it when it holds every snapshot named (ADR-0037).
- A relay's confirmed names cannot be changed yet: removing and adding it back is the way. The renewal path already follows `confirmed_names`, so an edit is a small later task.
- Names typed in a token (`POST /api/tokens`, `token create`) are not checked as confirmation is: a customer administrator can still type another site's name there, as since ADR-0025. Applying the same refusals to `POST /api/tokens` is a follow-up, and needs care for setups that run a relay on the server's own host.
- A waiting relay costs one small call every 10 seconds, and a fresh key per call.
