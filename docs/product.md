# MonitorFlow — Product, roadmap and business model

Status: draft, 2026-10-18.

## Goals for the first version

- Monitor Linux servers (Debian/Ubuntu and RHEL/Rocky) through lightweight Rust agents.
- Reach isolated networks and MSP customer sites through relay servers, with outbound-only connections.
- Host full apt and dnf mirrors and run scheduled update campaigns.
- Alert on sensor thresholds and let an AI propose or run shell remediation, with approval, audit and rollback. The AI is your choice: Anthropic, OpenAI, or any OpenAI-compatible API, including a local model so nothing leaves the network (ADR-0096).
- Ship a web interface from the first version, with graphs and a live view of the fleet.
- Notify by email, Slack, Teams, Discord and webhook when incidents open or resolve, approvals wait and updates fail, for everything, one customer or one site (ADR-0060, ADR-0077, ADR-0078), silence notifications for planned work (ADR-0079), and tell whoever is on call in person, in turns with covers for holidays (ADR-0081). When many things break at once, related incidents are sent together, at most once a minute per customer (ADR-0080), to the channels and to the person on call alike. Someone acknowledges an alert to say they are on it; when nobody does, a follow-up tells the next people in steps, the administrators after 15 minutes by default (ADR-0082).

## Who it is for

Two audiences with one product: a company running its own servers, and a managed service provider (MSP) running them for many customers. Neither should feel the product was made for the other.

- A company with one organisation sees no customers, no customer picker and no MSP vocabulary. Customers appear only once a second one exists.
- An MSP gets customers, sites, relays, per-customer routing and a PSA connection, without making the single-company case heavier.
- A customer of an MSP who is given access sees their own servers in plain words, never the MSP's other customers or its internal tools.

## Non-goals for the first version

- Windows agents (planned later).
- A custom time-series database.

## Roadmap

Each phase delivers something usable on its own. No dates are set.

| Phase | Name | Delivers |
| --- | --- | --- |
| 1 | Foundations | Protobuf schemas, certificate authority and enrollment, agent ↔ central connection over mTLS |
| 2 | Monitoring | Core sensors, VictoriaMetrics storage, thresholds and alerts, first web interface with graphs |
| 3 | Relays | Relay binary, chaining, store-and-forward buffering, MSP multi-tenancy |
| 4 | Packages and updates | apt and dnf mirrors, source configuration, scheduled campaigns in waves |
| 5 | AI remediation | Context gathering, risk tiers, approval queue, audit log, snapshot-based rollback |
| 6 | Windows agent | Sensor support and Windows update management; AI remediation with PowerShell came with Windows parity (ADR-0084) |

Detailed task lists per phase are in [roadmap.md](roadmap.md).

## Business model

Open core, like GitLab CE and EE (ADR-0100). The AGPL core holds every feature a company or an MSP needs to watch, alert on, update and fix its servers, and stays free. A short list of features for large or critical installs forms the **Enterprise edition**, sold by subscription: its code is public in `ee/`, under a source-available license that needs a subscription in production. High availability of the central server, running more than one, is the first and today only one. Everything published before stays AGPL.

Revenue comes from the Enterprise edition, from subscriptions that sell stability and support, and from services around the product.

### Subscription tiers (proposal)

| Tier | Update channel | Support | Enterprise edition | Target |
| --- | --- | --- | --- | --- |
| Community | No-subscription channel | Community forum | No | Homelabs, evaluation, small teams |
| Basic | Enterprise channel | Business-hours tickets, limited per year | No | Small companies |
| Standard | Enterprise channel | Business-hours tickets, unlimited, faster response | No | Production fleets |
| Premium | Enterprise channel | Priority response, remote assistance | Option | Critical environments, large MSPs |
| Enterprise | Enterprise channel | As Premium | Yes: a license key for a number of central servers and a period | High availability, large MSPs |

Support subscriptions are counted per managed server (or per relay site for MSPs). The Enterprise edition is counted per central server. Without a subscription nothing nags: a single central server never needs a license key, and only Administration › License mentions one. A license that expires stops adding central servers, never monitoring, alerts or the central servers already running.

### What the subscription sells

- **Enterprise edition** (Enterprise tier): more than one central server on one database, added with one command, so the platform survives the loss of a host (ADR-0089, ADR-0100).
- **Enterprise update channel** for the platform itself: builds tested longer than the no-subscription channel.
- **Validated distribution snapshots:** apt and dnf snapshots tested on reference servers before campaigns use them.
- **Support with response times**, by tier.

### Other revenue ideas

- Hosted edition: the project owner runs the central server; relays and agents stay on customer sites.
- Managed AI gateway with redaction and usage limits built in, billed per incident or per month.
- Professional services: deployment, migration from Zabbix or PRTG, custom sensors.
- Training and certification.
- MSP partner program: volume pricing and co-branding.

## Open questions

- [ ] Prices and support response times for each tier, and the price of the Enterprise edition per central server.
- [ ] Which features join the Enterprise edition after high availability. Each needs an ADR; a feature already in the core stays there.
- [ ] Final product name (MonitorFlow is the working name).
