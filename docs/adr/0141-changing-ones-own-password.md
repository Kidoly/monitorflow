# ADR-0141: Changing one's own password

Date: 2026-11-16
Status: accepted

## Context

A person could only get a new password from a password link that a global administrator issues (ADR-0059). Someone who simply wants to change theirs, because it leaked, because it was set in a hurry, or by habit, had to ask an administrator, and the administrator had no way to do it without the person giving up the old one in the open. The Preferences page (ADR-0137) is where each person changes what belongs to them; their password belongs there.

## Decision

**One route, for the person in a browser.** `POST /api/me/password` with `{"current_password", "new_password"}` replaces the signed-in person's password and answers 204. It sits under `/api/me/` with the preferences: the public API leaves that prefix out (ADR-0101), the handler refuses an API key with 403 and a display link (which has no session) the same way, and the CSRF rule of every other session route applies. There is no route to change someone else's password: administrators keep issuing links.

**The current password is required.** A session left open on a shared computer must not be enough to take the account over. A wrong current password answers 400 with a plain sentence rather than 401, so the web interface does not take it for an expired session and send the person to the login page.

**The same limits as signing in.** Each check of the current password is an attempt against the sign-in limits of ADR-0106, keyed by the person's username and the request's source: ten wrong passwords from one address or twenty on one account in ten minutes stop both this route and the login page with 429. Someone holding a stolen session cannot guess faster here than at the login page. A short new password, or one equal to the current one, is refused before any argon2 run and is not an attempt.

**The same strength rule.** The new password follows the rule of invitations and resets: at least 12 characters (`MIN_PASSWORD_LEN`). One rule everywhere; a stronger one would change all three in one decision.

**Other sessions end, this one stays.** The person changing a leaked password wants every other browser out, and the one they are using kept, so they are not asked to sign in again right after. This differs from a password link, which ends all sessions and starts a new one, because there the person was not signed in.

**Audited.** A change writes `user.password_set` with `via: self` and how many sessions it ended; a wrong current password writes `user.password_change_failed`, with `limit_reached` and the source when it used a limit up, like `auth.login_failed`. Neither entry holds a password.

**No external identity provider yet.** Every account signs in with a password kept by MonitorFlow, so the Preferences page always shows the form. When single sign-on comes, accounts it manages will show where to change the password instead, and this route will refuse them.

## Consequences

- People change their password themselves, from Preferences, without an administrator.
- A stolen session gains nothing toward the password: it still needs the current one, within the sign-in limits.
- Wrong current passwords count toward the login limits of the same account and address: someone mistyping it ten times waits like at the login page.
