# MonitorFlow — Product, roadmap and business model

Status: draft, 2026-10-18.

## Goals for the first version

- Monitor Linux servers (Debian/Ubuntu and RHEL/Rocky) through lightweight Rust agents.
- Reach isolated networks and MSP customer sites through relay servers, with outbound-only connections.
- Host full apt and dnf mirrors and run scheduled update campaigns.
- Alert on sensor thresholds and let an AI propose or run shell remediation, with approval, audit and rollback.
- Ship a web interface from the first version, with graphs and a live view of the fleet.
- Notify by email and webhook when incidents open or resolve, approvals wait and updates fail (ADR-0058).

## Non-goals for the first version

- Windows agents (planned later).
- A custom time-series database.
- A local or self-hosted AI model.
- Dedicated chat integrations. Email and webhook notifications are in the first version (ADR-0058, which revises this list); chat services are reached through webhooks.

## Roadmap

Each phase delivers something usable on its own. No dates are set.

| Phase | Name | Delivers |
| --- | --- | --- |
| 1 | Foundations | Protobuf schemas, certificate authority and enrollment, agent ↔ central connection over mTLS |
| 2 | Monitoring | Core sensors, VictoriaMetrics storage, thresholds and alerts, first web interface with graphs |
| 3 | Relays | Relay binary, chaining, store-and-forward buffering, MSP multi-tenancy |
| 4 | Packages and updates | apt and dnf mirrors, source configuration, scheduled campaigns in waves |
| 5 | AI remediation | Context gathering, risk tiers, approval queue, audit log, snapshot-based rollback |
| 6 | Windows agent | Sensor support and Windows update management |

Detailed task lists per phase are in [roadmap.md](roadmap.md).

## Business model

Following the Proxmox model, every feature lives in the AGPL core and nothing is locked behind a paywall. Revenue comes from subscriptions that sell stability and support, plus services around the product.

### Subscription tiers (proposal)

| Tier | Update channel | Support | Target |
| --- | --- | --- | --- |
| Community | No-subscription channel | Community forum | Homelabs, evaluation, small teams |
| Basic | Enterprise channel | Business-hours tickets, limited per year | Small companies |
| Standard | Enterprise channel | Business-hours tickets, unlimited, faster response | Production fleets |
| Premium | Enterprise channel | Priority response, remote assistance | Critical environments, large MSPs |

Subscriptions are counted per managed server (or per relay site for MSPs). Servers without a subscription show a notice in the interface but keep every feature.

### What the subscription sells

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

- [ ] Prices and support response times for each tier.
- [ ] Final product name (MonitorFlow is the working name).
