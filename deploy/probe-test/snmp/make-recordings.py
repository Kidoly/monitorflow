"""Writes the snmpsim recordings of the SNMP test (ADR-0149): a switch, a
UPS and a printer, made up for the test, with no vendor's or anyone's
data. A device is the recording's name, used as its community.

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


def switch():
    rows = system("Test 24-port gigabit switch, firmware 1.0", "floor-switch", "1.3.6.1.4.1.8072.3.2.10")
    if_t, ifx = "1.3.6.1.2.1.2.2.1", "1.3.6.1.2.1.31.1.1.1"
    rows.append("1.3.6.1.2.1.2.1.0|2|24")
    for i in range(1, 25):
        up = 1 if i <= 8 else 2
        rows += [
            f"{if_t}.1.{i}|2|{i}",
            f"{if_t}.2.{i}|4|GigabitEthernet0/{i}",
            f"{if_t}.3.{i}|2|6",
            f"{if_t}.5.{i}|66|1000000000",
            f"{if_t}.7.{i}|2|1",
            f"{if_t}.8.{i}|2|{up}",
            f"{if_t}.13.{i}|65|0",
            f"{if_t}.19.{i}|65|0",
            f"{if_t}.20.{i}|65|0",
            f"{ifx}.1.{i}|4|Gi0/{i}",
            f"{ifx}.15.{i}|66|1000",
        ]
        if i == 1:
            # Port 1 carries 125 kB/s in and 2 errors a second.
            rows += [
                f"{if_t}.14.{i}|65:numeric|rate=2,initial=10",
                f"{ifx}.6.{i}|70:numeric|rate=125000,initial=1000000",
                f"{ifx}.10.{i}|70:numeric|rate=25000,initial=1000000",
            ]
        else:
            rows += [f"{if_t}.14.{i}|65|0", f"{ifx}.6.{i}|70|0", f"{ifx}.10.{i}|70|0"]
    ent = "1.3.6.1.2.1.99.1.1.1"
    rows += [
        "1.3.6.1.2.1.47.1.1.1.1.7.1|4|Chassis",
        f"{ent}.1.1|2|8",
        f"{ent}.2.1|2|9",
        f"{ent}.3.1|2|0",
        f"{ent}.4.1|2|42",
        f"{ent}.5.1|2|1",
    ]
    return rows


def ups():
    rows = system("Test UPS 1500 VA, firmware 2.1", "ups-room", "1.3.6.1.2.1.33")
    base = "1.3.6.1.2.1.33.1"
    return rows + [
        f"{base}.1.1.0|4|Test UPS",
        f"{base}.1.2.0|4|1500 VA",
        # upsBatteryStatus: normal; writable, like the output source, so
        # the test can put it on battery with a SET.
        f"{base}.2.1.0|2:writecache|value=2",
        f"{base}.2.2.0|2|0",
        f"{base}.2.3.0|2:writecache|value=45",
        f"{base}.2.4.0|2:writecache|value=100",
        f"{base}.4.1.0|2:writecache|value=3",
    ]


def printer():
    rows = system("Test office laser printer", "printer-2", "1.3.6.1.4.1.8072.3.2.10")
    sup = "1.3.6.1.2.1.43.11.1.1"
    hr = "1.3.6.1.2.1.25.3.2.1"
    rows += [f"{hr}.1.1|2|1", f"{hr}.2.1|6|1.3.6.1.2.1.25.3.1.5", f"{hr}.3.1|4|Test office laser printer", f"{hr}.5.1|2|2"]
    for i, (name, maximum, level) in enumerate(
        [("Black Toner", 10000, 600), ("Cyan Toner", 6000, 4200), ("Waste Toner Box", -2, -3)], start=1
    ):
        rows += [
            f"{sup}.4.1.{i}|2|3",
            f"{sup}.5.1.{i}|2|3",
            f"{sup}.6.1.{i}|4|{name}",
            f"{sup}.7.1.{i}|2|13",
            f"{sup}.8.1.{i}|2|{maximum}",
            f"{sup}.9.1.{i}|2|{level}",
        ]
    return rows


for name, rows in [("switch", switch()), ("ups", ups()), ("printer", printer())]:
    with open(os.path.join(HERE, "data", f"{name}.snmprec"), "w") as out:
        out.write("\n".join(sorted(rows, key=oid_key)) + "\n")
