# ADR-0003: Provider-agnostic AI layer, Claude first

Date: 2026-10-18
Status: accepted

## Context

Remediation relies on an external AI API. Users will want to choose providers, and the project must not depend on one vendor. The first version calls an external API only; no local model.

## Decision

A single `AiProvider` trait in the server defines what the remediation loop needs: produce a diagnosis and the next proposed command from a structured context. One adapter per provider; the Claude API adapter ships first. Risk classification and approval policy live outside the provider and apply to every provider the same way.

## Consequences

- Provider output is treated as untrusted text: parsed, validated, classified by rules.
- Context sent to a provider passes through redaction first.
- Adding a provider means one new adapter and no change to the loop.
