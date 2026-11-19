# ADR-0163: What an import's connection may reach

Date: 2026-11-19
Status: accepted; extends ADR-0146 (connections) with ADR-0078's rule on private addresses

## Context

ADR-0146 lets an administrator of a site read another tool through its API: the central server makes the requests to the URL they type, without following redirects, with a timeout and a cap on answers. It said nothing of which addresses that URL may name.

An administrator of one site is not always someone who controls the central server: with several customers, it is a customer's administrator. Through a connection they could make the server request its own side of the network (its database and stores on the loopback or the container network, a cloud provider's metadata service) and learn from the answers and the error messages what is there. A reader that understands the answer, like the Prometheus one, even shows part of it in the preview.

Notification channels met the same question (ADR-0078): a channel saved by anyone but a global administrator reaches public addresses only, checked when the name is resolved so that a name cannot change after a check.

## Decision

**The same rule for connections.** A global administrator connects to any address, as before: the tool they move from is usually on their own network. Anyone else reaches public addresses only. An address written in the URL that is loopback, private, link-local, shared, documentation or the IPv6 form of one is refused before any request; a name is resolved by the channels' resolver, which returns public addresses only, and the connection uses what it returns. No proxy is used then, as a proxy would resolve the name itself. The error says the address is private and that only a global administrator can allow it.

The readers know nothing of the rule: `monitorflow-import` takes an optional guard in its context (`http::Guard`), which checks the URL of each request and gives the resolver its client uses; the server makes one from the channels' rule (`import::reach`).

## Consequences

- A customer's administrator moves from a tool on the Internet only; one on their own network is read by a global administrator, or uploaded as files, which need no connection.
- Single-company installations, where the administrator is a global one, see no change.
