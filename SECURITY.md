# Security policy

## Reporting a vulnerability

Report it privately through GitHub: on the Security tab, choose "Report a vulnerability", or go straight to the [advisory form](https://github.com/Kidoly/monitorflow/security/advisories/new). Do not open a public issue, discussion or pull request.

Say which version or commit you tested, how to reproduce the problem and what an attacker gains. Do not include real secrets or other people's data.

## What to expect

MonitorFlow is maintained by one person, on a best-effort basis, so there are no deadlines for a reply or a fix. Every report is read. Fixes go to `main` and ship in the next release, and the advisory is published once that release is out. Please keep the details private until then. You are credited if you want to be.

## Supported versions

Only the [latest release](https://github.com/Kidoly/monitorflow/releases/latest) gets security fixes; older releases get none, so update first. MonitorFlow is in early development. Reports against `main` are welcome.

## In scope

The boundaries MonitorFlow promises to hold, on a deployment that follows the guides in `docs/`:

- **First connection.** Getting a certificate, or a name you were not given, with or without an install command or a single-use token ([ADR-0004](docs/adr/0004-peer-identity-and-roles.md), [ADR-0007](docs/adr/0007-enrollment-bootstrap.md), [ADR-0073](docs/adr/0073-reusable-install-keys.md)). A relay gaining addresses no administrator confirmed ([ADR-0066](docs/adr/0066-relay-onboarding.md)).
- **Certificates and relays.** Renewing past a revocation ([ADR-0010](docs/adr/0010-certificate-renewal.md), [ADR-0029](docs/adr/0029-relay-and-enrollment-administration.md)). A relay speaking for servers outside its site or its subtree ([ADR-0028](docs/adr/0028-relay-renewal-and-chaining.md)), or making a server or relay below it trust a certificate authority the central server did not sign for ([ADR-0107](docs/adr/0107-signed-ca-bundle.md)).
- **Signed commands.** A server or relay running a command the central server did not sign, or one meant for another machine ([ADR-0031](docs/adr/0031-command-channel-and-updates.md)). Code running as the agent's or the relay's own user getting a root helper, or the Windows update service, to act without such a command, or twice on one ([ADR-0108](docs/adr/0108-root-helpers-verify-signed-commands.md)).
- **Signed releases and updates.** A server, relay or central server installing a release file or image the release key did not sign, or an older release without an explicit downgrade, whoever serves it ([ADR-0053](docs/adr/0053-agent-self-update.md), [ADR-0056](docs/adr/0056-server-update.md)). The server or web process reaching the updater's Docker socket, or choosing what it runs.
- **AI remediation.** A command running without the approval its risk tier needs, including when logs or AI output try to steer it ([ADR-0041](docs/adr/0041-risk-classifier.md), [ADR-0043](docs/adr/0043-incident-loop.md), [ADR-0045](docs/adr/0045-approval-queue.md)). One customer's data in another customer's AI request ([ADR-0040](docs/adr/0040-incident-context-and-redaction.md)).
- **Tenancy.** A user reading or changing another customer's or site's data, or acting above their role, through the API ([ADR-0026](docs/adr/0026-customers-and-tenancy.md)).
- **Web access.** Getting around sessions, the first-run setup link or one-time password links ([ADR-0006](docs/adr/0006-api-plain-http-behind-proxy.md), [ADR-0059](docs/adr/0059-users-in-the-interface.md), [ADR-0071](docs/adr/0071-first-run-setup.md)).
- **Plaintext listeners.** Port 80 serving anything beyond the ACME challenge, the `/trust/` page, the public CA certificate and redirects ([ADR-0068](docs/adr/0068-browser-certificate.md)). Mirror listeners exposing anything but their public files, or a tampered package passing the signature checks of the server installing it ([ADR-0030](docs/adr/0030-mirror-orchestration.md), [ADR-0034](docs/adr/0034-relay-mirror-sync.md)).
- **Audit.** With the application's database rights, changing or removing audit log rows, or altering a remediation history without detection ([ADR-0042](docs/adr/0042-remediation-incident-audit.md)).

## Out of scope

These are known limits or design choices, not vulnerabilities:

- What a role allows by design, and what someone with root on the central server can do with its online CA key, such as queuing signed commands ([ADR-0031](docs/adr/0031-command-channel-and-updates.md), [ADR-0053](docs/adr/0053-agent-self-update.md)).
- A compromised relay forging data for the servers of its own site or subtree ([architecture](docs/architecture.md#4-security-model)), or reading the commands it forwards ([ADR-0031](docs/adr/0031-command-channel-and-updates.md)).
- The `/trust/` page or its certificate being altered on the network before an administrator compares fingerprints ([ADR-0068](docs/adr/0068-browser-certificate.md)), and mirror traffic showing the network which packages a server downloads ([ADR-0030](docs/adr/0030-mirror-orchestration.md)).
- Releases before `v0.2.0` being unsigned, installers checking only checksums without OpenSSL 3, and Windows files having no Authenticode signature ([ADR-0053](docs/adr/0053-agent-self-update.md)).
- A short password in a shape the redaction rules do not recognise ([ADR-0040](docs/adr/0040-incident-context-and-redaction.md)), and a database superuser rewriting a whole remediation history ([ADR-0042](docs/adr/0042-remediation-incident-audit.md)).
- A machine installed before root helpers checked commands, whose service user was already someone else's when its helper first ran, and that user restarting the host through the `shutdown` sudo rule ([ADR-0108](docs/adr/0108-root-helpers-verify-signed-commands.md)).
- A wrong AI diagnosis, or a harmful command an approver accepted.
- Development setups: `deploy/docker-compose.dev.yml` and its default password, `scripts/dev-certs.sh`, and `api_cookie_secure = false`.
- Flaws in third-party software, such as PostgreSQL, Caddy, VictoriaMetrics, aptly, or the packages being mirrored. Report those upstream, unless the way MonitorFlow uses them is the problem.

## Verifying a release

From `v0.2.0` on, every release signs `SHA256SUMS` with an Ed25519 key held off every server, and agents, relays, the updater and the installers check `SHA256SUMS.sig` against the public keys built into them from `deploy/release-keys.pem`. `images.json`, listed in `SHA256SUMS`, names the images by digest, so the signature covers them too. To check a release by hand, see [docs/releasing.md](docs/releasing.md#images).
