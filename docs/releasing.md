# Releasing

How a maintainer publishes releases: the agent and relay files (ADR-0052, ADR-0055), the server, web and updater images (ADR-0056), and keeps the release signing key (ADR-0053).

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
3. `release.yml` checks that the tag matches `Cargo.toml`, builds the files and the images, pushes the images to GHCR, writes `images.json`, signs `SHA256SUMS` with `RELEASE_SIGNING_KEY` and publishes the GitHub Release. It fails without the secret, or when the secret's public half is not in `deploy/release-keys.pem`.

## Images

Each release pushes `ghcr.io/kidoly/monitorflow-server`, `monitorflow-web` and `monitorflow-updater`, tagged with the version, each a multi-platform index for `linux/amd64` and `linux/arm64` built on native runners. `images.json`, a release file listed in `SHA256SUMS`, names them by the digest of that index:

```
{"version": "0.2.0", "images": {"server": "ghcr.io/kidoly/monitorflow-server@sha256:…", "web": "…", "updater": "…"}}
```

Servers install a release's images by those digests only, after checking the signature (ADR-0056); the version tags are for people. The `release` job publishes nothing when an image fails to build, so a release always has its images.

GHCR creates each package as private on its first push. After the first release, make the three packages public, once: on GitHub, Packages, each `monitorflow-*` package, Package settings, Change visibility, Public. Until then servers cannot pull them without credentials, and their updates fail at the pull, with nothing changed.

A run without a tag builds the images for both platforms without pushing them, and its `images.json` names the amd64 builds, which no registry holds.

Check a published release by hand:

```
base=https://github.com/Kidoly/monitorflow/releases/download/v0.2.0
curl -fsSLO "$base/SHA256SUMS" -O "$base/SHA256SUMS.sig"
openssl pkeyutl -verify -pubin -inkey deploy/release-keys.pem -rawin -in SHA256SUMS -sigfile SHA256SUMS.sig
```

(`-inkey` reads the first key of the file; with several, put the one to check in a file of its own.)

If a release changes the systemd unit or the sudo rules, say in its notes that hosts must run the installer again: a self-update replaces the binary only. If it changes `deploy/docker-compose.yml` beyond the images, say that servers must run the server installer again: the updater changes images, not the compose file.

The server installer (ADR-0065) is the asset `install-server.sh`. It installs what `monitorflow-server-compose.tar.gz` holds, the files listed in `deploy/release-files.sh`. Add a file there when `deploy/docker-compose.yml` starts to need one, and a key to the `images.json` of the workflow when it starts to need an image: the installer pins every key `name` as `NAME_IMAGE`. Since a release's `install-server.sh` lands at `releases/latest/download/` as soon as it is published, a release without images, or with private packages, breaks new installs: make the packages public right after the first one.

Sites without Internet access import the assets of the release, all of them, into their central server (`monitorflow-server agent-release import`, ADR-0054). Keep the asset names fixed: the server, the installer and the token page's commands look for them by name.

## Rotating the key

1. Generate the new key as above and append its public half to `deploy/release-keys.pem`; keep the old one.
2. Release with the **old** key. Agents updated to this release trust both keys.
3. Once the fleet runs it, `gh secret set RELEASE_SIGNING_KEY` with the new key, remove the old public key from `deploy/release-keys.pem`, and release.

Agents older than the release of step 2 must update through it, or be reinstalled with the installer, which carries the keys of its own release. If the old key leaked, do the same without waiting, and reinstall hosts that cannot update through step 2: whoever holds the old key can sign for every agent that still trusts it.
