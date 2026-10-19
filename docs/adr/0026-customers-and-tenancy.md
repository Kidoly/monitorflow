# ADR-0026: Customers and tenancy enforcement

Date: 2026-10-20
Status: accepted

## Context

In MSP mode each customer sees only its own sites, and tenancy is enforced in the API, never only in the frontend (architecture §9). Until now the data model had sites but no customers, and role grants were global or on one site (`user_roles.site_id`). Every handler checked access its own way: `readable_sites()` in the listings, `can_read_site()` after a lookup in server detail and the metric proxy, `authorize()` in sensor configs, thresholds and incidents. A new handler could forget one of them.

Most installs have one organisation and should not have to know customers exist.

## Decision

**Schema** (migration `20261020120000_customers`):

- `customers (id, name UNIQUE, created_at)`.
- `sites.customer_id NOT NULL REFERENCES customers`. The migration creates the customer **`default`** with the fixed ID `00000000-0000-0000-0000-000000000001`, moves every existing site to it, and keeps it as the column default. Code that inserts a site without a customer (enrollment's `--create-site`, tests) keeps working, and a single-tenant install never sees anything but `default`.
- Site names stay **unique across customers**. The `site` label of every series in VictoriaMetrics is the site name (ADR-0013), and the metric proxy scopes queries by it (ADR-0018). Two customers with a site `paris` would share series.
- `user_roles.customer_id`, with `CHECK (customer_id IS NULL OR site_id IS NULL)`. A grant is global (both null), on a customer (all its sites, including ones created later), or on a site. The uniqueness constraint now covers `(user_id, role, customer_id, site_id)`. Existing grants are unchanged.
- **A site does not move between customers** through the API or the CLI. Everything that belongs to a site (servers, sensor configs, thresholds, incidents through servers, series through the site name) follows the site, so a move done in SQL would be consistent, but it changes who sees past data. It needs its own audited command when an MSP asks for it.

**Enforcement in one place.** `api::access` turns grants into sites once per request: a customer grant is expanded to the customer's sites when the session is loaded. Handlers only use:

- `CurrentUser::readable_sites()` / `modifiable_sites()`, a `Scope` (`All` or `Only(ids)`) bound to `($1 OR site_id = ANY($2))` in listings;
- `access::authorize(user, site, modify)` for one resource: no role at all is 403, a missing or unreadable site 404, a readable one the user may not change 403 (ADR-0019's rules, unchanged).

The metric proxy looks the server up, passes its site through `authorize`, and only then sends a query with that server's `peer` and `site` filters (ADR-0018). A server outside the user's sites is 404 and nothing reaches VictoriaMetrics.

**API, additive only:**

- `GET /api/auth/me` roles gain `customer_id`.
- `GET /api/sites` and `GET /api/servers` (and server detail) items gain `customer_id` and `customer_name`.
- `GET /api/customers`: `{"customers": [{"id", "name", "site_count"}]}` sorted by name. A user sees every customer with a global grant, the customers granted to them, and the customers of their site grants. `site_count` counts only the sites the user reads, so a site grant does not reveal how many other sites its customer has.
- `POST /api/customers` `{"name"}`: global administrators only; 201 the customer, 409 on a duplicate name. Audited `customer.create`.
- `POST /api/customers/{id}/sites` `{"name"}`: administrators of that customer (global or customer grant); a customer the user cannot see is 404, one the user sees without that right 403. 201 the site, 409 on a duplicate name. Audited `site.create`.

The web interface ignores the new fields for now; `web/` types do not change.

**CLI.** `monitorflow-server customer add --name`, `customer list`; `user add` takes `--customer` or `--site` to scope its first role; `user grant --username --role [--customer | --site]` adds a grant; `token create --customer` names the customer of a site made with `--create-site`, and refuses an existing site of another customer. User and grant management stays CLI-only for now.

## Consequences

- A new handler that filters with `readable_sites()` or calls `authorize` gets customer grants for free. The tenancy integration test enumerates every route of the router, so a route without a tenancy case fails it.
- Loading a session costs one more query when the user has customer grants.
- A customer grant covers sites created after login from the next request on, since grants are resolved per request.
- Unique site names across customers are a constraint MSPs will notice ("paris" for two customers). Lifting it needs a customer label on series first.
- The AI layer (phase 5) must build each request from one customer's data; `sites.customer_id` is what it groups by.
