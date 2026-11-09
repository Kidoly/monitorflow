# MonitorFlow Enterprise edition

This directory holds the code of the Enterprise edition (ADR-0100). Today that is one feature: running more than one central server on one database, for high availability, added with `install-server.sh --join`.

Everything outside `ee/` is free software under the AGPL (`../LICENSE`), and so is every release published before this directory existed.

## Licence

The files in `ee/` are under the MonitorFlow Enterprise License (`LICENSE` in this directory): the code is public, anyone may read, change, build and test it, and production use needs a subscription and its license key. **The license is a draft, to be reviewed by a lawyer before the first Enterprise release.**

As an additional permission under section 7 of the GNU AGPL, the copyright holder of MonitorFlow allows you to combine its AGPL code with the code in this directory into one program, and to convey that program, as long as the code in this directory stays under its own license and the rest under the AGPL. (Draft, reviewed with the license.)

A server built without the Cargo feature `enterprise` (`cargo build -p monitorflow-server --no-default-features`) contains AGPL code only. It does everything except add a central server.

## Contents

- `LICENSE`: the MonitorFlow Enterprise License.
- `license-keys.pem`: the public keys that verify license keys, built into the server.
- `crates/server-ee`: license keys, and letting an additional central server start.

Contributions are welcome, under the same Contributor License Agreement as the rest of MonitorFlow (`../CONTRIBUTING.md`).
