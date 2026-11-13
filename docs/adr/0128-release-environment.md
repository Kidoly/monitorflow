# ADR-0128: The release signing key lives in a gated environment

Date: 2026-11-12
Status: accepted; amends ADR-0053

## Context

ADR-0053 stored the release signing key as the repository secret `RELEASE_SIGNING_KEY`, read only by the publishing run of `release.yml`. That condition is in the workflow file, and the workflow file is whatever the branch being run says. A repository secret is given to any workflow run on any branch pushed to the repository: a write collaborator, a stolen token with the `workflow` scope or a bot that can push could add a workflow that prints the key, without going through the protection of `main`. Nothing protected `v*` tags either, so anyone who could push could tag a commit and get a signed release published.

A security audit (2026-11-12) found both. Whoever holds the key can sign a binary that every agent installs as root (ADR-0053), so the key deserves more than the branch rules.

## Decision

- **The secret moves to a GitHub environment, `release`.** It has one required reviewer, the maintainer, and its deployment rule admits `v*` tags only. A job reads an environment's secrets only once it runs in that environment, which GitHub allows only on a matching ref and after approval. A workflow pushed on a branch cannot get past either check. The repository secret is deleted.
- **Only the publishing run enters the environment.** The `release` job sets `environment: release` when the run publishes (a `v*` tag push) and none otherwise. Pull request and manual runs keep signing with a throwaway key, never wait for approval and need no secret.
- **A tag ruleset protects `v*`.** Only repository admins may create, move or delete those tags, so a push right alone cannot start a release.

The setup commands are in `docs/releasing.md`.

## Consequences

- Each release waits for the maintainer to approve the `release` job in the Actions tab. The builds and the image pushes run first. The signing and the GitHub Release wait for the approval.
- The images job still pushes to GHCR on a tag with `GITHUB_TOKEN`, outside the environment. Servers install images by the digests listed in the signed `images.json` only (ADR-0056), so an image pushed without a signed release is installed nowhere.
- The workflow change has to land after the secret is in the environment: until then, a tag would run the job in an environment without the key, and it fails before publishing anything.
