# Releasing

How a maintainer publishes agent releases (ADR-0052) and keeps the release signing key (ADR-0053).

## The signing key, once

Agents install a new binary only when `SHA256SUMS.sig` verifies against a key in `deploy/release-keys.pem`, compiled into them. The private key stays off every server and host; GitHub Actions reads it from a repository secret when a tag is pushed.

On a trusted machine, with OpenSSL 3:

```
umask 077
openssl genpkey -algorithm ed25519 -out monitorflow-release.pem
openssl pkey -in monitorflow-release.pem -pubout >>deploy/release-keys.pem
gh secret set RELEASE_SIGNING_KEY --repo Kidoly/monitorflow <monitorflow-release.pem
```

Then:

1. Keep `monitorflow-release.pem` offline, for example encrypted on two separate media (`openssl pkey -in monitorflow-release.pem -aes256 -out monitorflow-release.enc.pem`), and delete the clear copy. Without it nobody can publish a release the agents accept; with it anyone can.
2. Commit `deploy/release-keys.pem` with the new public key, in a pull request like any other change. Releases built from that commit trust it.
3. Optionally, move the secret to a GitHub environment with required reviewers and add `environment:` to the `release` job of `.github/workflows/release.yml`, so a tag alone cannot sign.

Check the pair before the first release: `openssl pkey -in monitorflow-release.pem -pubout` must print the key you committed.

## Publishing

1. Set `version` in `[workspace.package]` of `Cargo.toml` and merge it.
2. Tag the merge commit `v<version>` and push the tag: `git tag v0.2.0 && git push origin v0.2.0`.
3. `release.yml` checks that the tag matches `Cargo.toml`, builds the files, signs `SHA256SUMS` with `RELEASE_SIGNING_KEY` and publishes the GitHub Release. It fails without the secret, or when the secret's public half is not in `deploy/release-keys.pem`.

Check a published release by hand:

```
base=https://github.com/Kidoly/monitorflow/releases/download/v0.2.0
curl -fsSLO "$base/SHA256SUMS" -O "$base/SHA256SUMS.sig"
openssl pkeyutl -verify -pubin -inkey deploy/release-keys.pem -rawin -in SHA256SUMS -sigfile SHA256SUMS.sig
```

(`-inkey` reads the first key of the file; with several, put the one to check in a file of its own.)

If a release changes the systemd unit or the sudo rules, say in its notes that hosts must run the installer again: a self-update replaces the binary only.

## Rotating the key

1. Generate the new key as above and append its public half to `deploy/release-keys.pem`; keep the old one.
2. Release with the **old** key. Agents updated to this release trust both keys.
3. Once the fleet runs it, `gh secret set RELEASE_SIGNING_KEY` with the new key, remove the old public key from `deploy/release-keys.pem`, and release.

Agents older than the release of step 2 must update through it, or be reinstalled with the installer, which carries the keys of its own release. If the old key leaked, do the same without waiting, and reinstall hosts that cannot update through step 2: whoever holds the old key can sign for every agent that still trusts it.
