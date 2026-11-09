# ADR-0106: Limits on what a caller can make the server do

Date: 2026-11-09
Status: accepted

## Context

A security audit (2026-11-09) found places where someone without an account, or with the lowest role, could make the central server do unbounded work:

- `POST /api/auth/login` had no limit. Each attempt runs argon2id (19 MiB) on the blocking pool, and each failure appends a row to the audit log, which is never deleted from.
- `POST /api/setup` and `POST /api/password-link/use` hashed the new password before looking at the link, so any request cost a hash.
- The body of a webhook's refusal was read whole before 200 characters of it were kept, and any signed-in user can point their own on-call webhook anywhere and test it at will.
- A log line had no size limit before redaction, and redaction jobs were queued without bound.

ADR-0059 and ADR-0071 describe the links; nothing in them changes. This ADR adds the limits.

## Decision

**Sign-in is limited per source address and per account**, in memory per instance of the central server, with the limiter of enrollment (ADR-0007) and screens (ADR-0099). The source is the address the reverse proxy appends to `X-Forwarded-For`; IPv6 counts by /64.

- **10 wrong attempts per source in 10 minutes**, on any accounts. Then the source gets 429 until the window ends.
- **20 wrong attempts per account in 10 minutes**, from all sources together. Then that name gets 429. The account is the name as typed, lowercased, whether it exists or not: an unknown name fills up and answers exactly like a real one, so the limit tells nobody which accounts exist.
- **A source that signed in to an account before is still let in when the account is full**, within its own 10 attempts. The server remembers the last 4 such sources per account. Wrong passwords sent from elsewhere therefore do not lock an administrator out of their usual place; from a new place they wait at most 10 minutes after the attempts stop. A password link (ADR-0059) and the server CLI never go through sign-in and keep working.
- **Counted before verified.** An attempt takes its place in both counts before anything is looked up, and gives it back when the password was right. Requests arriving together cannot all reach argon2, and a right password never counts.
- **Refused attempts cost nothing and write nothing.** No database query, no argon2, no audit row. The wrong attempt that used a limit up says so in its own `auth.login_failed` row (`limit_reached`: `source` or `account`, and the `source` address), so the audit log shows the attack and stops growing with it: at most 10 rows per source and 20 per account in 10 minutes.

**A one-time link is checked before a password is hashed.** Setup and password links validate the form, parse the token and look for a live row first; without one the request is refused unhashed. The link is still consumed by the same `DELETE ... RETURNING` after hashing, so of two requests with one link only one succeeds: the first look decides nothing but whether to hash.

**A refusal's body is read up to 4 KiB**, in chunks, by one helper used by notification webhooks, chat channels and outgoing events. Testing a personal contact is limited to 6 a minute per user.

**A log line is cut to 8 KiB before it is queued** (twice the 4 KiB stored, so a line that redaction shortens still fills it), on a character boundary. Redaction runs on all that is kept, as before. A stream has at most 4 batches in redaction at once; a further batch is dropped whole, counted in the stream's dropped lines, and the next batch admitted carries a line saying how many were lost, like the rate limit's.

Rejected:

- **Locking the account for everyone.** The classic lockout lets anyone who knows a username keep its owner out for as long as they keep trying.
- **Only a per-source limit.** An attacker with many addresses could then try one account without end.
- **A growing delay instead of 429.** Holding requests open costs connections, and the queue behind an attacker delays the real user as a lockout would.
- **CAPTCHA or an external rate limiter.** Dependencies and a third party for something a counter does.
- **An audit row per refused attempt.** It is the unbounded growth the limit is there to stop.
- **Limits in the reverse proxy.** The API must hold on its own; the proxy is replaceable.

## Consequences

- Guessing a password online is bounded to 20 attempts per account in 10 minutes, whatever the number of sources.
- Someone at an address an account signed in from learns, once that account is full, that it exists: they get 401 where an unknown name gets 429. Accepted: it needs the address of a real user of that account.
- Counts are per instance and reset on restart, like the other limits. With several instances (ADR-0100) each has its own.
- Without a reverse proxy in front (development), every client is one source and shares its 10 attempts.
- The web interface shows the 429 message as it is: "Too many sign-in attempts. Wait a few minutes and try again."
- A user behind the same address as an attacker (a shared NAT) is refused with them until the window ends.
