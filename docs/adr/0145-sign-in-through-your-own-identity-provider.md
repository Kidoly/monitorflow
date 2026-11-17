# ADR-0145: Signing in through your own identity provider

Date: 2026-11-17
Status: accepted; amends ADR-0059, ADR-0101 and ADR-0141

## Context

Every account signs in with a password MonitorFlow keeps (ADR-0059). Companies and MSPs already keep their people, their groups and their second factor in an identity provider: Microsoft Entra ID, Google, Okta, Keycloak, authentik, Zitadel or an Active Directory or OpenLDAP directory. They want to give access to MonitorFlow the way they give it to their other tools: put someone in a group, and take them out when they leave. Keeping a second list of accounts by hand is how a former employee keeps access.

The owner's rules apply: one-click paths and sensible defaults, MSP wording only once there is a second customer, everything the web interface does through the public API, secrets sealed and never shown back, and no plaintext link.

## Decision

### Two kinds of provider

**OpenID Connect**, with any provider that follows the standard, self-hosted ones included. The authorization code flow with PKCE (S256), the provider's details read from its discovery document (`/.well-known/openid-configuration`), and a confidential client (a client secret, sent as the provider's discovery document prefers, `client_secret_basic` by default) or a public one (no secret, PKCE alone).

**LDAP directories, over TLS only**: LDAPS (`ldaps://`, port 636) or StartTLS (`ldap://`, port 389, upgraded before anything is sent). `ldap://` without StartTLS is refused when saving, and a directory that does not offer StartTLS fails the connection: MonitorFlow never sends a bind over plain LDAP. The server binds with a service account, searches the person with a filter in which what they typed is escaped (RFC 4515), then binds as them with their password; an empty password is refused before any bind, as many directories take it for an anonymous bind.

Both trust the usual public certificate authorities, plus a certificate authority pasted with the provider for one that runs on a private one. Every name is checked.

Several providers may be set up at once, each with its own name, for an MSP whose customers each have their own. Most installs have one.

### The OpenID Connect sign-in

1. The login page shows "Sign in with <name>" for each provider turned on. It is a link to `GET /api/auth/oidc/{id}/start`, not a form, so the `form-action 'self'` of the web interface's policy (ADR-0124) does not stop the redirect.
2. `start` makes a random `state`, a `nonce` and a PKCE verifier, seals them with the provider's ID and an expiry of ten minutes in a cookie (`__Host-mf_sso`, `HttpOnly`, `Secure`, `SameSite=Lax` so the browser sends it back when the provider redirects, `Path=/`), with the secrets key of ADR-0125 and its own associated data. Nothing is stored in the database, so a flood of starts costs nothing and any central server (ADR-0100) can finish a sign-in another one began. It redirects to the provider's authorization endpoint with `response_type=code`, the S256 challenge, `state`, `nonce` and the scopes.
3. The provider sends the browser to `GET /api/auth/oidc/callback`. One address for every provider: `<web_url>/api/auth/oidc/callback`, shown on the provider's page to copy into the provider. The server opens the cookie, requires the same `state`, clears the cookie, and trades the code for tokens with the verifier.
4. **The ID token is checked** before anything else is read from it: a JWS whose `alg` is RS256, RS384, RS512, PS256, PS384, PS512, ES256, ES384 or EdDSA (never `none`, never HS256 with the client secret), signed by a key of the provider's JWKS with that `kid` (or the only key of a fitting type when there is no `kid`); `iss` exactly the discovered issuer; `aud` containing the client ID, and `azp` equal to it when there are several audiences; `exp` in the future and `iat` not in the future, with a minute of leeway; `nonce` equal to the sealed one; a non-empty `sub`. Signatures are verified with `ring`, which the server already uses: no JWT library is added.
5. **Key rotation**: the JWKS is kept in memory for an hour per provider. A token signed by a `kid` the server does not know fetches it again at once, at most once every 30 seconds per provider, so a provider that rotates its keys is followed without a restart and a forged `kid` cannot make the server fetch in a loop.
6. Groups come from the ID token's groups claim (`groups` by default, configurable), and from the userinfo endpoint when the ID token does not carry it. The username is `preferred_username`, else `email`, else `sub`.
7. The callback ends with a 303 to `/` and the session cookie, or to `/login/?sso=<reason>` with a reason the login page puts in words.

Discovery documents and endpoints must be `https`. Requests to the provider follow no redirects and give up after 10 seconds.

### LDAP sign-in

The login form stays the same. A known account signs in with its own provider: a local one with its password as before, a directory's with its directory. A name no account has is tried against each directory turned on, in the order they were added; the first that knows it and gives access creates the account. A directory that took the password while none of the person's groups gives access answers 403 `no_access`, a new error code, with that reason: only someone who knows the password learns it. The sign-in limits of ADR-0106 apply before any directory is asked, as they do before argon2, and one argon2 run is still made whatever the account, so the time does not say which kind it is.

Groups are the entries of the group base that name the person as a member (`member`, `uniqueMember` or `memberUid` by default; the filter is configurable), read by their `cn` (configurable). **Nested groups**, when turned on: Active Directory resolves them itself through the `LDAP_MATCHING_RULE_IN_CHAIN` rule, used when the root DSE says it is Active Directory; any other directory is walked by MonitorFlow, group of groups after group, at most 10 levels and 500 groups.

### Accounts and roles

**Accounts are created at the first sign-in**, named after the provider's username. An account belongs to one provider (`users.identity_provider_id`) and one identity there (`external_subject`: the OpenID Connect `sub`, or the directory's username in lower case). **An identity is never joined to an existing account by its name**: a provider whose user picks the name of a local administrator would otherwise take that account. When the name is taken, the sign-in is refused and the audit log says so; an administrator renames or removes the other account.

**Roles come from groups.** Under Settings › Sign-in, each provider has a list of group mappings: a group name, a role, and for an MSP where it applies (everything, a customer or a site). The place is hidden while there is one customer and one site: a single company maps a group to a role, nothing else. Group names are compared without regard to case. At each sign-in the account's roles are replaced by those its groups give, and the groups are kept on the account (`external_groups`), so a change of mappings applies to every account of the provider at once, without waiting for them to sign in again. **Someone in no mapped group is refused**, at sign-in and, when a mapping change leaves them without any role, by the end of their sessions.

Roles of these accounts are not granted by hand: the Users page shows where they come from, and the routes that grant or revoke a role, or issue a password link, answer 409 for them. Disabling an account still keeps its person out whatever their groups; removing one lets them back at their next sign-in if their groups still give access.

**Multi-factor authentication stays with the provider.** MonitorFlow does not add its own: a provider that asks for a second factor asks for it before it sends the person back.

**Accounts of a provider have no password.** Preferences says where they change it instead, and `POST /api/me/password` answers 409 for them (amends ADR-0141). `GET /api/auth/me` and each user of `GET /api/users` name the account's provider (`provider`: its ID, name and kind, `oidc` or `ldap`), `null` for a password kept by MonitorFlow.

### One local administrator keeps a password

The rule of ADR-0059, "no change may leave the server without an enabled global administrator who has a password", already counts only accounts with a password, which no provider's account has. It now holds against providers too: their accounts never count, so a provider that is down, misconfigured or removed never locks everyone out, and the Users page refuses to remove, disable or demote the last local one. The login page keeps the password form under the provider's buttons for that reason: when a provider is on, it is folded behind "Sign in with a password", for local accounts and directories.

### Configured from the web interface, tested, audited

Settings › Sign-in (Access group), for global administrators. A provider is saved turned off. **"Test connection"** checks it without saving: for OpenID Connect the discovery document (issuer equal to the address given, `https` endpoints, S256 offered when the provider lists methods), the JWKS (at least one usable key), and the client's ID and secret, by trading a code that does not exist and reading the provider's answer (`invalid_client` means a wrong ID or secret; `invalid_grant` means they were accepted); for LDAP the TLS connection, the service account's bind and a search of the user base, and with a username and password typed in the test, the whole sign-in with the groups found and the roles they give. **Turning a provider on runs the same test on the server and refuses with its reason when it fails**, as AI does (ADR-0064); so does changing a provider while it is on.

**Secrets** (the client secret, the service account's password) are sealed like the other secrets of the server (ADR-0125), in `identity_providers.secret`, and never returned: views say `has_secret`. An empty secret in a change keeps the stored one, unless the issuer or the directory's address changed: a stored secret is never sent to another address than the one it was entered for (as the AI key, ADR-0096).

**Audited**: `identity_provider.create`, `.update` (old and new settings, `secret_changed`, never the secret), `.delete`, `.groups` (old and new mappings); `auth.login` with `via` and the provider; `auth.login_failed` with `no_group`, `username_taken`, `provider_off` or `provider_error`; `user.create` and `user.roles` (old and new roles) made by the provider, a `system` actor named `identity_provider:<id>`.

Turning a provider off or removing it ends its accounts' sessions. Removing it keeps its accounts, without roles and without a way to sign in, so their history keeps a name; an administrator removes them or gives them a password link.

### Public API

`/api/identity-providers` (list, create, read, change, remove), `/api/identity-providers/test` and `/api/identity-providers/{id}/groups` (read and replace the mappings) are in `/api/v1`, documented in the OpenAPI document, for a global administrator's key of reach "Everything" and access "Administer", area "administration". The sign-in itself, `GET /api/auth/providers`, `/api/auth/oidc/{id}/start` and `/api/auth/oidc/callback`, is for people in a browser and stays out under `auth` (ADR-0143).

## Consequences

- People get in and out of MonitorFlow with their group in the company's provider; a former employee loses access when they lose the group, at their next sign-in or at once when the mapping changes.
- The local administrator's password remains the way in when the provider is down. It must be kept somewhere safe: it is the only account that does not depend on the provider.
- Two new tables, `identity_providers` and `identity_provider_groups`, three columns on `users`, and one more sealed column.
- New dependency: `ldap3`, with rustls and ring, the TLS stack the server already uses.
- `deploy/sso-test.sh` runs Keycloak, authentik and OpenLDAP (LDAPS and StartTLS, with a test certificate authority) in local containers and signs in through each of them in Chromium.

## Plan

Stacked pull requests, `sso/…`, each on the one before:

1. `sso/adr`: this ADR, the glossary, the architecture and the migration.
2. `sso/config`: provider settings and their checks, TLS toward providers, the sealed column.
3. `sso/jws`: JWS and JWK verification with `ring`.
4. `sso/oidc`: discovery, the key cache with rotation, the code flow and the ID token's checks.
5. `sso/accounts`: accounts made at first sign-in, roles from groups.
6. `sso/ldap`: the directory sign-in over LDAPS and StartTLS, nested groups.
7. `sso/providers-api`: Settings › Sign-in in the API and its OpenAPI document.
8. `sso/sign-in`: the sign-in routes, the login form's directories, the Users rules, `provider` in `/api/auth/me`.
9. `sso/api-tests`: the API tests against a fake provider over TLS.
10. `sso/web-login`: the login page, Preferences and Users.
11. `sso/web-settings`: Settings › Sign-in.
12. `sso/e2e`: `deploy/sso-test.sh` with Keycloak, authentik and OpenLDAP in a browser, its CI job, the guides and the roadmap.
