# ADR-0083: Limits on what a relay reads from its upstream

Date: 2026-11-02
Status: accepted

## Context

A relay copies mirror snapshots from its upstream over plain HTTP (ADR-0034) and reads their metadata to know what to fetch: apt `Release` and `Packages` lists, dnf `repomd.xml` and `primary` metadata, compressed with gzip, xz, bzip2 or zstd. A review found that nothing bounded the decompressed size of those files: a list of a few tens of kilobytes could expand to gigabytes, in memory for an apt line or an XML event, or in time for the rest. It also asked what the relay trusts before checking a signature.

The answer to the second question is: everything, because the relay never checks a signature. ADR-0034 decided so: the relay has no OpenPGP implementation, the key at `/keys/` comes over the same channel, and the agents check the signatures with the key they received pinned over mTLS (ADR-0032). The checksums a relay compares each file against come from `Release` and `repomd.xml`, which it reads unverified. They catch a damaged or half-copied file, and they make sure a list or a package is only used once its bytes are those its parent file names, but they do not stop whoever can change the upstream's answers, since that party can change the parent file too. So every byte the relay parses is untrusted input, and limits are what protects it.

## Decision

**Every decompressed metadata file is capped at 512 MiB**, whatever its compression, plain files included. Reading byte 512 MiB + 1 fails the sync with an error that names the file and says, in plain words, that it expands to more than any real repository's metadata and that the upstream's copy is damaged or was tampered with. `current` stays where it was, as for any failed sync, and the next cycle tries again. The largest metadata files a relay parses today are Fedora's `primary.xml` (198 MB for rawhide in November 2026, 56 MB for EPEL 9), Ubuntu's universe `Packages` (73 MB for noble) and Debian's main `Packages` (65 MB for sid): 512 MiB leaves 2.5 times the largest. Larger files exist in repositories, like Fedora's `filelists.xml` (856 MB) or Ubuntu's `Contents` (829 MB), but the relay copies them without reading them, so their size is bounded by the listing and `mirror.max_snapshot_bytes` only. The cap is a constant, not a setting: nobody can pick a better value without reading this, and a repository that outgrows it is a code change to make once for everyone.

The cap also bounds memory. The apt reader holds one line at a time and quick-xml one event at a time, but neither limits a line or an event: a single tag a gigabyte long would be held whole. Since they read from the capped decoder, neither can hold more than 512 MiB. The decoders bound their own working memory too: xz streams may ask for at most 128 MiB (`xz -9` needs 65 MiB, the format allows 1.5 GiB), and ruzstd refuses windows over its default of 128 MiB. gzip and bzip2 use fixed, small windows.

**The signed root files are capped at 16 MiB**, down from 64 MiB: Ubuntu's `Release`, the largest seen, is 255 KB, and `Release` is read whole into memory. `repomd.xml` is a few kilobytes.

**A response whose `Content-Length` is larger than expected is refused before its body is read**: larger than the listed size of a file, or than the limit of a file whose size nothing gives in advance (root files, the index, the key, agent releases). The body is still counted as it arrives, since the header can be absent or lie, and a download that goes past the expected size is cut off, as before.

**What is checked before use.** Nothing is parsed from a file the relay has not either fetched within a fixed limit (the root files) or matched against the size and checksum its parent lists (every `Packages` list and the `primary` metadata, and every package). A file that does not match is deleted and never read. This was already so; tests now cover it.

**A snapshot without its signatures is refused.** The server signs every snapshot it publishes (`aptly publish` writes `InRelease` and `Release.gpg`, the dnf snapshot gets `repomd.xml.asc`, ADR-0030), and a relay copies them unchanged, so a snapshot missing one is broken or tampered with, and the agents would refuse it. The relay used to copy it anyway; it now fails the sync, saying the snapshot is not signed. A signature that is present but wrong is still copied unchanged, since the relay cannot tell; the agents refuse it.

**Repositories without signature checks.** The relay behaves the same whatever the agents check: it never relies on a signature, so all of the above applies to every snapshot. On the agents, apt always checks `Release` against the pinned key. dnf checks each package's vendor signature (`gpgcheck`, on unless a source turns it off, ADR-0032) and not `repomd.xml.asc`; a dnf source with `gpgcheck` off trusts its transport, the relay included. That is the administrator's choice for that source, and nothing the relay can make up for.

Checking signatures on the relay, with the key pinned over its mTLS stream, stays a follow-up, as ADR-0034 noted. It would not remove the need for these limits: the files a signature covers must still be read within bounds to be checked.

## Consequences

- Tampered metadata can still make a relay fetch and read up to 512 MiB per list, and fail; it can no longer exhaust its memory or run a sync forever on one file.
- A relay refuses a repository whose metadata outgrows 512 MiB; the error says why, and raising the constant is a release.
- A relay refuses a snapshot without its signature files. Snapshots published by the server always have them.
- Sync errors are in the relay's log only, as before (ADR-0034); showing them in the web interface needs the relay to report them on its stream, a follow-up.
