---
title: "How to draw a live map of your network"
description: "Get a map of a site or a customer drawn from your relays, dependencies and switches' LLDP and CDP neighbours, coloured by incidents, arranged by hand on your floor plan, and shown on a wall screen."
slug: draw-network-maps
lang: en
translation: dessiner-des-cartes-reseau
---

# How to draw a live map of your network

A network map shows what is linked to what, in the colours of the moment: the switch in red, the servers behind it unreachable, the uplink at 95 %. MonitorFlow draws it from what it already knows, and you arrange it as you like.

## Make a map

1. Open **Network maps** in the sidebar and click **"New map"**.
2. Give it a name and choose what it is a map **of**: a site, or a customer with every site.
3. Click **"Make the map"**. Every server, device and relay of that place is drawn and laid out at once.

Links come from:

- **relays**: the central server, its relays, and what each relay carries or checks;
- **dependencies** you set or imported ("these servers are behind this switch");
- **neighbours**: what your switches and routers report over LLDP and CDP;
- optionally, **addresses found by network scans** that nothing watches yet, in grey.

## Read your switches' neighbours

Devices watched over SNMP can say what is plugged into each of their ports. In the map editor, click **"Read neighbours now"**: MonitorFlow asks each SNMP device of the place, from the relay or central server that checks it. A neighbour that is a server or device of the map becomes a link between them, showing the traffic of the switch's port; any other is drawn in grey, "Not watched". Read them again after recabling.

## Arrange it

Click **"Edit the map"**, then:

- **drag** nodes where you want them; **"Lay out again"** starts over;
- **"Add a zone"** for a floor, a room or a rack: drag it, resize it from its corner, name and colour it;
- **"Add a label"** for any text;
- **"Draw a link"**, then click two nodes: give it a label and, for a switch port or a server interface, the interface whose traffic it shows;
- select a node and **"Leave off the map"** to hide it; the list at the bottom brings it back;
- set a **background**: a floor plan or a photo, PNG, JPEG or WebP up to 1 MiB, with its opacity.

Click **"Save"**. Servers added later are laid out under what you placed.

## Read the colours

- A node has the colour of its state: red critical, amber warning, grey offline, unreachable or not watched, green OK.
- A link is as bad as the worse of its two ends: red when one is critical, offline or unreachable or its interface is down, amber on a warning or when the interface is used at 90 % or more.
- A link with an interface shows its traffic and is thicker the busier it is.

Click a node to open its page. The map zooms with the wheel or the **+** and **−** buttons and moves by dragging; **"All"** shows the whole map.

## Put it on a wall screen

1. Make a dashboard, add a panel, and in **Show** choose **"A network map"**, then the map. Give the panel the full width.
2. On the dashboard, make a **display link** and open it on the screen.

The screen needs no sign-in, shows the map read-only and always fitted, refreshes every 30 seconds and says plainly when it lost its connection. For a meeting, **"Full screen"** on the map's page is enough.

## Who can do what

Everyone who can see a site sees its maps; operators make and change them. A customer's map shows each person only the sites they can see. Everything here is in the public API, under `/api/v1/maps`.
