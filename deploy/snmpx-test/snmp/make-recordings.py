"""Writes the snmpsim recordings of the custom SNMP test (ADR-0157): a
firewall and a NAS with objects under their makers' enterprise numbers,
made up for the test, with no vendor's or anyone's data. A device is the
recording's name, used as its community.

- firewall: a session count that moves (a gauge), an inbound byte
  counter that wraps at 2^32 every few minutes, a firmware text the test
  changes with a SET, and an HA state (an enumerated integer, writable).
- nas: a system state (1 normal, 2 failed, writable), a temperature in
  tenths of a degree, and a RAID table of two volumes with a state each.

Run with: python3 -I make-recordings.py (from this directory)."""

import os

HERE = os.path.dirname(os.path.abspath(__file__))


def oid_key(line):
    return [int(x) for x in line.split("|", 1)[0].split(".")]


def system(descr, name, object_id):
    return [
        f"1.3.6.1.2.1.1.1.0|4|{descr}",
        f"1.3.6.1.2.1.1.2.0|6|{object_id}",
        "1.3.6.1.2.1.1.3.0|67:numeric|rate=100,initial=360000",
        f"1.3.6.1.2.1.1.5.0|4|{name}",
    ]


def firewall():
    rows = system("Test firewall, firmware 7.2", "fw-edge", "1.3.6.1.4.1.12356.101.1.1")
    sys = "1.3.6.1.4.1.12356.101.4.1"
    return rows + [
        # Firmware version, a text the test changes.
        f"{sys}.1.0|4:writecache|value=v7.2.5",
        # Processor and memory use, percent.
        f"{sys}.3.0|66|12",
        f"{sys}.4.0|66|48",
        # Sessions: a gauge that climbs from 1,500 and wraps at 2,000.
        f"{sys}.8.0|66:numeric|min=1000,max=2000,initial=1500,rate=3,wrap=1",
        # HA state: 1 master, 2 backup, 3 failed; writable.
        "1.3.6.1.4.1.12356.101.13.2.1.1.15.1|2:writecache|value=1",
        # Inbound bytes of port 1: 20 MB a second, so a Counter32 wraps in
        # about three and a half minutes.
        "1.3.6.1.2.1.2.2.1.10.1|65:numeric|rate=20000000,initial=4290000000,wrap=1,max=4294967295",
    ]


def nas():
    rows = system("Test NAS, DSM 7.2", "nas-files", "1.3.6.1.4.1.6574.1")
    syno = "1.3.6.1.4.1.6574"
    rows += [
        # systemStatus: 1 normal, 2 failed; writable.
        f"{syno}.1.1.0|2:writecache|value=1",
        # A temperature in tenths of a degree.
        f"{syno}.1.2.0|2|415",
    ]
    for i, (name, status) in enumerate([("Volume 1", 1), ("Volume 2", 1)], start=1):
        rows += [
            f"{syno}.3.1.1.1.{i - 1}|2|{i - 1}",
            f"{syno}.3.1.1.2.{i - 1}|4|{name}",
            f"{syno}.3.1.1.3.{i - 1}|2:writecache|value={status}",
        ]
    return rows


for name, rows in [("firewall", firewall()), ("nas", nas())]:
    with open(os.path.join(HERE, "data", f"{name}.snmprec"), "w") as out:
        out.write("\n".join(sorted(rows, key=oid_key)) + "\n")
