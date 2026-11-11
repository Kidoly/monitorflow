# ADR-0109: The server runs on a database role that owns nothing

Date: 2026-11-10
Status: accepted; amends ADR-0042, ADR-0051, ADR-0056, ADR-0058, ADR-0087 and ADR-0100; implemented in three stages (see the end)

## Context

A security audit (2026-11-09) found that the production compose file gives the server the role `monitorflow`, which the PostgreSQL image creates as the superuser and which, having run the migrations, owns every table.

Triggers keep `audit_log` and the remediation history append-only, but an owner or a superuser can switch triggers off. So whoever had the server's database rights (through an SQL injection, the server process, or `POSTGRES_PASSWORD`) could run `ALTER TABLE audit_log DISABLE TRIGGER USER`, change or delete rows, and switch the triggers back on. `audit_log` has no hash chain, so nothing would show. The same role could rewrite a remediation history and recompute its chain, and run programs in the database container with `COPY ... FROM PROGRAM`. `SECURITY.md` counts changing audit rows "with the application's database rights" as a vulnerability.

The schema's comments, architecture §6, ADR-0002 and ADR-0042 all said production gives the application role INSERT and SELECT only. Nothing created such a role.

With an added central server (ADR-0100) it was worse: `--add-server` publishes PostgreSQL on the first machine's address, `pg_hba.conf` accepted any role from anywhere, and the join file carried the superuser's password to every added machine.

## Decision

### Two roles

- **`monitorflow`, the owner.** It stays the superuser the image created and the owner of the schema. It runs migrations, and the updater uses it through the container's local socket for backups and restores. It is the only superuser of the PostgreSQL server, so it cannot be demoted, and nothing is gained by renaming it: it stops being what the server runs on.
- **`monitorflow_app`, the server's role.** No superuser, no `CREATEDB`, `CREATEROLE`, `REPLICATION` or `BYPASSRLS`, owner of nothing, no right to create anything in the schema. It may connect to the one database, SELECT, INSERT, UPDATE and DELETE on tables, and use sequences, except:

| Table | Withheld |
| --- | --- |
| `audit_log`, `remediation_incident_events` | UPDATE, DELETE |
| `remediation_incidents` | DELETE |
| `incident_events` | UPDATE |
| `_sqlx_migrations` | INSERT, UPDATE, DELETE |

TRUNCATE is never given. The list is `db::roles::WITHHELD`; a migration that adds an append-only table adds a line. Not owning a table, the role cannot disable or drop its triggers, replace their functions, or give itself rights; not being a superuser, it cannot set `session_replication_role` or use `COPY` with a program or a file.

The triggers stay: they stop the owner's mistakes, and their messages are the ones tests and operators know.

### Who creates the role: the migration step, every time

`monitorflow-server migrate` connects with `database_migrate_url` (`MONITORFLOW_DATABASE_MIGRATE_URL`), applies migrations, then, in one transaction, creates the role named in `database_url` if it is missing, sets its password to the one in that URL, takes back whatever it had and gives the rights above, on what exists and, as default privileges, on what the owner creates later. Running it again changes nothing, so it runs before every start. The name and the password are sent as bound values, never in statement text.

This is why there is no init script: the image runs those only on an empty data directory, which no installed server has. One step serves new and installed servers, repairs a right changed by hand, follows a changed password, and gives a restored database its rights back.

It refuses a `database_url` whose role is the owner, a member of it, or the owner of a table.

### How the server opens its database

| Settings | Role of `database_url` | The server |
| --- | --- | --- |
| `database_migrate_url` set | any | migrates and prepares the role through it, then runs on `database_url` |
| not set | superuser or owner | migrates and runs on it, as before, with a warning at start; refuses when `database_require_restricted_role` is true |
| not set | neither | only checks that the database has exactly this version's migrations, and refuses to start otherwise |

A warning and not a refusal by default, because the middle row is the state of every installed server while it is updated (below), and of development and tests, where `DATABASE_URL` stays one role that can create databases. The compose files set `database_require_restricted_role`, so a deployment on them cannot fall back to the owner unnoticed.

### In the compose files

A one-shot service, `db-migrate`, runs `monitorflow-server migrate` with both URLs before the server starts. The `server` service gets only `monitorflow_app`'s URL: the process that serves the API and every agent holds no credential of the owner, which covers a compromised server process and not only an injection. `POSTGRES_APP_PASSWORD` in `deploy/.env` is generated like `POSTGRES_PASSWORD`.

`pg_hba.conf` accepts `monitorflow_app` on the `monitorflow` database from anywhere, with TLS and its password, and the owner only from the networks the database container is on (`samenet`), where `db-migrate` is. PostgreSQL reads the file once and a replaced file is a new one to a container that mounted the old, so the `postgres` service carries a label that changes with the file: bringing the compose file up then recreates the container.

### Installed servers

- **From the web interface.** The old updater starts the new server on the old compose file and `.env` (ADR-0087): the middle row above, it migrates and warns. The new updater, before it puts the new compose files in place, adds `POSTGRES_APP_PASSWORD` to `.env` when missing; bringing the files up runs `db-migrate` and restarts the server on its own role. If that fails, the previous files come back as ADR-0087 says and the server is on the owner again; the extra `.env` line and the unused role harm nothing. Nothing is dropped or demoted, so no step is irreversible.
- **From the installer or offline.** The installer adds the line before it starts the new files.
- **From a checkout.** Compose stops with "set POSTGRES_APP_PASSWORD in .env" until the operator adds it.
- **Later updates.** The updater runs `db-migrate` with the new image before it recreates the server, since the server no longer migrates. A failure there is a failed update, rolled back as before.

### Added central servers

The join file carries `POSTGRES_APP_PASSWORD` and no longer `POSTGRES_PASSWORD`. An added central server runs on `monitorflow_app` and never migrates: the first central server does, and it is updated first.

A central server added before this holds the owner's password and no other. After the first central server is updated, the operator makes a new join file with `--add-server` and runs `--join` again on each added machine, which replaces its credentials. Until then it cannot open new database connections, since the owner is no longer accepted from another machine. Keeping the owner reachable from the network for one more release was rejected: that exposure is the finding.

### Backups and restores

`pg_dump` is unchanged. A dump holds rights but not roles, which belong to the PostgreSQL server. The restore therefore passes `--no-acl`, and `db-migrate`, which runs when the restore starts everything, recreates the role when the PostgreSQL server is new and gives the rights again. Without it a restore on a new machine would stop on a GRANT to a role that does not exist.

## Consequences

- With the server's database rights, audit rows and remediation history can be added and read, not changed or removed, and no program runs in the database container.
- The owner, and so anyone with root on the Docker host or `deploy/.env`, can still rewrite everything. Anchoring chain heads outside the database stays ADR-0042's follow-up.
- `samenet` is as narrow as a file that does not know the networks' addresses can be. Measured on Docker Engine with the default port publishing: through the published port, the machine's own address and so another machine's are refused for the owner, while a container of another Docker network on the first central server arrives as the gateway of the shared network and is accepted, with the password. That is local to a machine whose root already reaches the database.
- Starting the server takes one more short container, and an update one more step that can fail.
- An added central server needs a new join file once. Change `POSTGRES_PASSWORD` afterwards (`ALTER ROLE monitorflow PASSWORD ...`, then `.env`): the old one was on those machines.
- A new append-only table must be added to `WITHHELD`; default privileges would otherwise give the role UPDATE and DELETE on it, leaving its triggers as the only guard.
- Development and tests keep one role and see the warning at each start.

## Stages

1. This ADR, the settings, `db::roles`, the `migrate` command and how the server opens its database. Nothing changes while one URL is set.
2. The updater: the `.env` line, the `db-migrate` step, `--no-acl`. Nothing changes until the compose file has the service.
3. The compose files, `pg_hba.conf`, the installer, the join file and the guides.
