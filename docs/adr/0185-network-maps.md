# ADR-0185: Network maps

Date: 2026-11-24
Status: accepted; builds on ADR-0099 (dashboards on a screen), ADR-0149 (SNMP devices), ADR-0152 (dependencies), ADR-0154 (network discovery) and ADR-0178 (dashboard visualizations)

## Context

People moving from PRTG, Zabbix or The Dude expect a picture of their network: what is behind what, in the colours of the moment, on the wall of the operations room. MonitorFlow already knows most of that picture: relays and what they carry (ADR-0072), dependencies set by people or imported (ADR-0152), devices and their templates (ADR-0149), and what network scans found (ADR-0154). What it did not know is which port of which switch a machine is plugged into: switches say it over LLDP and CDP.

A map is only useful if it costs nothing to start and stays right afterwards. Drawing every node by hand is what makes maps in other tools go stale; a layout entirely automatic is unreadable for anything but a small network, and people want their floor plan, their racks and their words on it.

## Decision

**A map belongs to a site or to a customer as a whole**, like a shared dashboard (ADR-0095): readers of the site, or of the customer as a whole, see it; operators change it. What it shows is resolved for the viewer on every read: a customer's map shows a viewer of one of its sites that site only, whatever it stores. Several maps per place are allowed ("Ground floor", "Data centre"). A single company sees its one customer as "Every site" (ADR-0104).

**What draws it, without anyone drawing.** Nodes are the servers, devices and relays of the place, and the central server when there are relays. Links come from sources each map turns on or off:

- **Relays**: the central server to each relay, a relay to what it carries and what it checks (the implicit dependencies of ADR-0152).
- **Dependencies**: from the parent to the child, a check's dependency drawn between their servers.
- **Neighbours**: what SNMP devices last said over LLDP and CDP (below).
- **Found addresses**, off by default: what the latest run of each scan of the place found and nothing watches yet, as for the "Add" proposals of ADR-0154. A dismissed address is not drawn.

Two links between the same nodes are one, whatever said them; the neighbour's ports are added to it. A neighbour that is a node of the map (by its management address, else its host name without the domain, case or CDP serial) links to it; any other is drawn as a grey node, "Not watched". A map shows at most 2,000 nodes and 500 found addresses and unmonitored neighbours, and says when it left some out.

**LLDP and CDP through `snmp_read`.** Reading neighbours is six walks of ADR-0157's one-off `snmp_read` per device (LLDP-MIB's remote, management address and local port tables, CISCO-CDP-MIB's cache, ifName and ifDescr), from the device's probe, so relays need nothing new: an old relay reads them as well. The server parses the rows and names the device's port as its interfaces check names it (ifName, else ifDescr, ADR-0149), so the link can show that interface's traffic. Neighbours are kept per device (`network_neighbors`, replaced at each read, with `network_neighbor_reads` for when and why not) and every map of the place uses them. They are read when an operator asks, "Read neighbours now" on a map, for at most 50 devices at a time, 8 at once: a topology changes when someone patches a cable, and a scheduled walk of every switch every few minutes would cost more than it tells. The text is from the network: bounded, without control characters, and shown as text.

ARP and FDB tables (which MAC address is behind which port, and which address has which MAC) are not read yet: matching them to servers needs the MAC addresses of every agent and device, and a switch's FDB lists every machine of the network behind its uplink. They are the next step for access switches without LLDP.

**What people add is a document**, stored as JSON (`network_maps.document`, version 1) and validated on every write: where each node is placed and which are left off the map, zones (named, coloured rectangles: a floor, a rack, a room), links drawn by hand with a label and the interface whose traffic they show, labels of three sizes, and where the background lies with its opacity. Keys name nodes (`central`, `server:<id>`, `relay:<id>`, `host:<address>`, `neighbor:<name>`), so a placement survives a server's rename and a node that is gone is simply not drawn. Saving passes the revision read, and 409 says someone saved in between, as for dashboards.

**Layout in the browser, with no library.** What nobody placed is laid out by the web interface: a block per site side by side, each drawn top-down, a relay or dependency parent above its children and a switch, router or firewall above what it sees, the central server above everything, unlinked nodes in a grid under their site's tree, rows ordered to cross less. It is about 150 lines, deterministic (the same map lays out the same on the editor and on a screen) and keeps loops of dependencies from hanging the page. Nodes placed by hand stay where they are; new ones are laid out under them. "Lay out again" places everything afresh. A force layout was not chosen: it moves at each load and reads worse for trees, which networks mostly are. Libraries such as ELK or dagre are heavy for what this needs, and newer versions target browsers the screens of ADR-0099 lack.

**Live states.** A node takes its fleet state (ADR-0130), a relay online or offline, the central server is OK, and what nothing watches is grey. A link is as bad as the worse of its two ends and of its interface: red when an end is critical, offline or unreachable, or its interface is down (`oper_up` 0); amber when an end has a warning or the interface is used at 90 % or more of its speed, the threshold of the switch template; dashed grey when nothing watches either end. A link with an interface shows its traffic in and out, in bits per second, and is thicker the busier it is, from one query to VictoriaMetrics per map that matches SNMP interface metrics and the agents' `net` metrics alike. A map without metrics still draws.

**Background.** One image per map, PNG, JPEG or WebP, at most 1 MiB, checked by its first bytes; never SVG, which could carry scripts. It is served with its type and `nosniff`, read by viewers of the map, set and removed by operators, audited.

**On a wall screen: a dashboard panel.** A map is a new panel kind, `map`, with `map_id` (ADR-0178): a dashboard of one map panel three columns wide fills a screen, and the display links of ADR-0099 show it without sign-in, read-only, revocable, with stale data said. Nothing new about links, sessions or rights: the panel's data is the map as the link's maker sees it, within the dashboard's sharing, and a map of another place than the dashboard's is refused when saving. The screen fetches the background through one more route of its session, `POST /api/display/map-background`, only for a map a panel of one of its dashboards shows. On the map's own page, "Full screen" shows it in the browser for a meeting.

**Drawing.** SVG of our own, as the charts (ADR-0178): nodes shaped by kind (hexagon for the central server, diamond for a relay, square for a device with a mark of its template, circle for a server, dashed for what is not watched) and filled with their state's colour, from the theme's tokens in light and dark. The map never scrolls: it zooms with the wheel or buttons and moves by dragging; on a screen it always fits. Clicking a node opens its page; on a screen nothing is a link. Text has a halo so it reads over a background.

**API.** Everything is in `/api/v1` and its OpenAPI document, area `dashboards`: `GET`/`POST /api/v1/maps`, `GET`/`PUT`/`DELETE /api/v1/maps/{id}`, `GET /api/v1/maps/{id}/live`, `GET`/`PUT`/`DELETE /api/v1/maps/{id}/background`, `POST /api/v1/maps/{id}/neighbors`. Creating, changing, deleting a map and setting its background are audited (`network_map.create`, `.update`, `.delete`, `.background`); reading neighbours is not, as it changes no setting.

## Alternatives rejected

- **Maps drawn only by hand**, as many tools start. Stale by the first new server.
- **Storing the computed nodes and links.** They would drift from the fleet; computing them on read follows renames, removals and new servers.
- **A separate display link for maps.** A second kind of credential to review and revoke; a dashboard panel reuses what ADR-0099 decided.
- **New relay envelopes for neighbours.** `snmp_read` walks already go through relays and give the raw rows; parsing on the server keeps relays simple.
- **Inferring dependencies from neighbours.** A map shows what is linked; whether a link should silence alerts stays a person's decision (ADR-0152).

## Consequences

- A new map shows the place's servers, devices and relays, laid out, with relay and dependency links, at once; neighbours appear after the first read.
- Positions, zones, links, labels and background are kept per map; what nobody placed follows the fleet.
- A wall screen shows a map through a dashboard and a display link, with nothing new to secure.
- Not in this change: ARP and FDB tables, reading neighbours on a schedule, links between maps (a node opening another map) and per-link thresholds.
