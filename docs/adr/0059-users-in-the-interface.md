# ADR-0059: Users, customers and roles in the interface

Date: 2026-10-29
Status: accepted

## Context

Users, grants and customers could only be created with `monitorflow-server user add`, `user grant` and `customer add` inside the server container (simplicity audit, B6). An MSP could not onboard a customer or a colleague from the browser, and a forgotten password needed a shell.

## Decision

**Who.** Administrators of everything (a global administrator grant) manage every account and create and rename customers. Customer and site administrators keep managing sites and install tokens, not people: users are not owned by a customer, so a customer administrator changing them could reach other customers.

**One-time password links.** An administrator never types or sees anyone's password. Inviting a user creates the account with its first role and **no password**, and returns a link `<web origin>/welcome/#<secret>`. The person opening it chooses a password and is signed in. The same link, issued for an existing user, is the password reset. The mechanism is the setup link's (ADR-0057): a 256-bit secret in the URL fragment, only its SHA-256 stored in `password_links`, deleted as it is used so two requests cannot both succeed. Differences:

- one live link per user, replacing the previous one;
- valid 7 days, so an invitation survives a weekend;
- returned once, in the API response, to the administrator, who copies it to the user. It is never logged. Email delivery can come with notifications;
- the current password keeps working until the link is used; using it ends every session of the user;
- a disabled user's link is void, and none can be issued until it is enabled again.

**Disable and remove.** Disabling keeps the account and its roles, ends its sessions and blocks sign-in, and can be undone. Removing deletes the account, its roles, sessions and link. What the user did stays: the audit log names it by ID (and `user.remove` records the username), and the rows it created (commands, campaigns, enrollment tokens, install keys, remediation rules and decisions, Proxmox settings) keep existing with their user reference set to NULL. The foreign keys become `ON DELETE SET NULL` for that, and the single-use trigger of enrollment tokens allows exactly that change on a used token. The username is free again afterwards.

**No lockout.** A change that would leave no *active global administrator* (enabled, with a password, with an administrator grant on everything) is refused with 409: revoking that grant, disabling or removing the account. Such changes take a lock on `user_roles` so two administrators cannot remove each other at once. An administrator cannot disable or remove their own account at all. An invited administrator does not count until they set a password.

**Audit.** `user.create` (with the first role), `user.link_issued`, `user.password_set`, `user.grant`, `user.revoke`, `user.disable`, `user.enable`, `user.remove`, `customer.rename`.

The CLI commands keep working; `user add` still sets a password directly.

Rejected:

- **The administrator sets a temporary password.** Two secrets to pass instead of one, and the administrator knows it.
- **Removal as a soft delete.** A hidden state the interface would have to explain, and a username that stays taken. Disabling already covers "keep it, but block it".
- **Customer administrators managing their customer's users.** Needs users owned by customers; later, if MSPs ask.

## Consequences

- A colleague or a customer is onboarded from the browser: name, role, copy the link.
- `users.password_hash` is nullable; sign-in treats a missing password like a wrong one.
- History rows can lose their user reference; the audit log is the record of who did what.
- Install keys a removed user created keep enrolling hosts until they expire or are revoked: they belong to the site, not the person. The token and key lists say "the server CLI or a removed user" when no creator is left.
