---
title: "How to know what each server is made of and what it runs"
description: "See the model, serial number, processors, memory, disks, network cards, BIOS, installed software, services, Windows features and licences of every server, find which servers have an old OpenSSL, follow changes over time and export them as CSV."
slug: hardware-and-software-inventory
lang: en
translation: inventaire-materiel-et-logiciel
---

# How to know what each server is made of and what it runs

Before a security notice, a warranty claim or a budget meeting, the questions are the same: which servers have this version of OpenSSL, which ones run on that disk model, when does this one's warranty end, what changed on it last week. MonitorFlow's agent answers them without any setup.

## What the agent reads

From MonitorFlow 0.12, every agent sends its server's inventory when it starts, when something changes, and at least once a day. It only reads; on Linux it runs as its own user.

| | Linux | Windows |
| --- | --- | --- |
| Model, maker, serial number, BIOS | yes (from the firmware) | yes |
| Processors, total memory | yes | yes |
| Memory modules (slot, size, part number) | no: they need root to read | yes |
| Disks (model, serial number, size, firmware, SSD or hard disk) | yes | yes |
| Network cards (MAC address, speed, addresses, driver) | yes | yes |
| Installed software and versions | the system's packages | installed programs |
| Services and how they start | yes | yes |
| Windows features, licences | | yes |

On Linux, the serial numbers are readable by root only: the agent's service copies them for the agent when it starts, as it already does for the hardware ID. An agent started by hand, or inside a container, reports none.

## A server's inventory

Open a server and choose the **Inventory** tab. It shows the hardware, the disks, memory modules and network cards, the installed software with a filter and an **Export CSV** button, the services, Windows features and licences, and the **Changes**: what was added, removed or changed, newest first, packages included. Changes are kept for a year.

**Warranty.** Makers give warranties only through services that need a key, so MonitorFlow does not look them up. An operator of the site types the end date and a note (the contract, the level of service) with **Set…**. A warranty ending within 90 days is shown in orange, an expired one in red.

## Searching the whole fleet

**Inventory** in the sidebar searches every server you can see:

- **Software**: type a name, like `openssl`, and a version in **Version below**, like `3.0.15`, to list the servers still on an older one. It looks in Linux packages, Windows updates, winget, Flatpak and Snap apps, and Windows programs. Versions are compared the way Debian does, which suits RPM and Windows versions too.
- **Disks, Models and BIOS, Processors, Network cards, Services, Windows features, Licences**: leave the text empty to list everything, or type part of any field (a disk model, a BIOS version, a service name).

**Export CSV** downloads every match, up to 100,000 rows, with one column per piece and the details as `name=value` pairs.

## Through the API

Everything above is in the public API (tag `inventory`): `GET /api/v1/servers/{id}/inventory`, `GET /api/v1/servers/{id}/inventory/changes`, `PUT /api/v1/servers/{id}/warranty` and `GET /api/v1/inventory?kind=software&q=openssl&version_below=3.0.15` (add `format=csv` for the file).

## If something is missing

- **"No inventory yet"**: the agent is older than 0.12, or has not reconnected since its update. It sends its inventory as soon as it connects.
- **No serial number on Linux**: the agent does not run from MonitorFlow's service, or the firmware has none (many virtual machines report a placeholder, which is left out).
- **No memory modules on Linux**: reading them needs root; the total memory is shown instead.
