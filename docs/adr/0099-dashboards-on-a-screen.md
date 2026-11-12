# ADR-0099: Dashboards on a screen

Date: 2026-11-16
Status: accepted

## Context

Dashboards (ADR-0095) are pages people open. Teams also want one on a wall: a TV in the operations room, a screen at reception, a Raspberry Pi behind a monitor, an old tablet on a shelf. Such a screen has nobody in front of it to sign in, scroll or click; it is read from three metres; it stays on for months; and when it loses its network it must not keep showing the last numbers as if they were live, which is worse than a black screen.

A browser signed in as a person does not do: sessions end after 12 hours (ADR-0069), and a screen in a shared room signed in as an administrator hands that administrator's session to anyone who walks up to it. Whatever replaces the sign-in is a credential that sits in a TV's settings for a year, so it must be worth little if it leaks, the way ADR-0082's acknowledge link is.

The owner's rule applies: as simple as possible to set up and to use, in plain words, for a single company and an MSP alike.

## Decision

**Two ways to show a dashboard on a screen.**

- **Show on a screen**, a button on every dashboard, ready-made ones included: the same dashboard full screen in the signed-in browser, at `/display/?id=…`. For a meeting room or a laptop on a projector, for the day. Anyone who sees the dashboard may.
- **A display link**, for a screen that stays on: a link that needs no sign-in, made on the dashboard's page by an administrator, at `/display/#<secret>`.

Both show the same page.

**The page.** No navigation, no edit controls, no links: nothing on it leads anywhere. The dashboard's panels fill the screen, in its three-column grid, every row the same height, so a dashboard of four rows fits a TV as one of two does; the text scales with the screen's smaller side (`vmin`), so 1920×1080 and 3840×2160 show the same layout, the second one sharper. Figures are large; charts are redrawn for the space they get, with their lines thick and each line's last value beside its name instead of a cursor nobody can move. It is dark by default, which suits a screen that is always on; `?theme=light` before the `#` makes it light for a bright room. The dashboard's name is at the top left; the clock and "Updated 14:02:31" at the top right. A mouse moved over it shows two buttons for three seconds: "Full screen" and, signed in, "Close". A kiosk browser is full screen already.

**Refresh.** Every 30 seconds, the period of dashboards (ADR-0095). The answers are cached 15 seconds by exact query and the end of each range is aligned to its step, so screens showing the same dashboard ask VictoriaMetrics once between them, and a hundred screens cost what a few people looking at dashboards cost. Faster would show nothing new: metrics arrive every 10 to 60 seconds. The page also re-reads what it shows every 5 minutes, so a dashboard edited or a link turned off shows without anyone touching the screen. The clock is the server's: each answer carries the server's time (`now`), and a TV whose clock is wrong still shows the right time.

**Stale data is said, not shown as live.** When the last answer is older than twice the refresh period (60 seconds), whatever the reason (the network, the server, the reverse proxy, a request that hangs), a banner across the top says "Data from 14:02 — this screen has lost its connection to MonitorFlow. It keeps trying." and the panels are dimmed. The age is measured on the screen's own monotonic clock from when the answer arrived, so a wrong TV clock cannot hide it. When the connection comes back, the banner goes at the next answer. A panel whose metric backend failed says so in the panel, as on the dashboard page. A link that stopped working replaces the panels with one sentence saying why (below).

**Cycling: a list in the link, not a new object.** A display link shows one dashboard, or up to 12 in turn, each for the time the administrator chose (10 seconds to an hour; 30 seconds by default). The list and the time live in the link (`display_link_dashboards`, in order). A separate "playlist" object would need its own page, its own sharing and its own rights, and every screen has exactly one list anyway; making a second link with another list is as quick. A dashboard deleted is taken out of the lists that show it; a list left empty shows "nothing to show". The signed-in "Show on a screen" shows one dashboard; a screen that cycles is a screen on a wall, and has a link.

**The display link.**

- **Who makes them: administrators of the dashboard's place.** A customer's administrator for that customer's dashboards and its sites', a site's administrator for that site's, a global administrator for any; an administrator also for their own "Only me" dashboards. Not operators or viewers: a link is a credential that outlives the session, and handing one out is an administrator's decision, like an install key (ADR-0073). Every dashboard of a link must be one the maker may link. A link to an "Only me" dashboard is refused when its panels name a site, customer or server the maker does not administer, or every server while the maker reads more than they administer: its screen would show less than its page without saying so. Never with an API key: a link reads as the person, beyond what the key reaches (ADR-0101, amendment of 2026-11-09).
- **It acts as its maker, checked on every request.** Each request loads the maker again with their roles of now and shows only the link's dashboards they may still link: a maker who loses the administrator role or the customer sees "nothing to show", one who is disabled finds the link dead, one who is deleted takes the link with them. The panels' servers are resolved for the maker exactly as for a person viewing the dashboard (ADR-0095), within the dashboard's sharing, so a customer's link never shows another customer's data whatever is stored. An "Only me" dashboard has no sharing to keep it within, so its screen reads as the maker narrowed to their administrator grants: only the servers of sites they administer, never those they only read (amendment below).
- **Read-only, and only its dashboards.** The screen's session opens three routes: what to show, and the data of one of its dashboards. The dashboard specs come from the database, never from the screen, so a leaked link cannot ask for other panels; every other route answers 401 to it, as to anyone without a session. The page shows no link to anything.
- **The secret stays out of logs.** 256 random bits, of which only the SHA-256 is stored, in the fragment after `#`, which browsers never send (ADR-0059, ADR-0082). The page posts it once to `POST /api/display/start` in the body, and gets a screen session: another random token, in an `HttpOnly`, `SameSite=Strict` cookie of its own (`__Host-mf_display`), for one hour. The page repeats this when the session ends, so the secret crosses the network once an hour, never in a URL a proxy logs. The secret stays in the address the screen was given, which is what lets a TV that restarts come back by itself.
- **Expiry, chosen when it is made:** 1 year (the default), 30 days, or never. A year means a screen goes dark once a year at worst, on a date shown next to the link; "never" is there for those who would rather turn links off by hand.
- **Turned off from the dashboard page,** by its maker or anyone who may link one of its dashboards. Screens stop at their next request, within 30 seconds, and say "This display link was turned off or has expired. Ask an administrator for a new one." The page lists each link with where it is ("Reception TV", a name the administrator gives), what it shows, who made it, when it expires and when a screen last used it, written at most once a minute.
- **Rate limited**, in memory per instance of the central server: 10 links that do not work per source address in 10 minutes (the source being the address the reverse proxy appends to `X-Forwarded-For`), 30 openings per link an hour, 30 requests per screen a minute. A screen needs 8 at most. Over a limit the screen gets 429 and tries again by itself. The secrets have 256 bits; these limits are not what stops guessing, but what stops a leaked link or a broken screen from loading the server.
- **Audited**: `display_link.create` and `display_link.revoke`, with the name, dashboards, time per page and expiry; never the secret. Screens' requests are not audited, as viewing a dashboard is not.

**Safe if leaked**: whoever has the link sees those dashboards, as its maker sees them now, until it expires or is turned off, and nothing else: no other dashboard, no server page, no action. That is what a photo of the screen would show. The dashboard page shows when it was last used, which tells an administrator that a link turned off long ago is still being tried, or that a link is used from somewhere it should not be.

**MSP.** Nothing to set: a customer's administrator links that customer's dashboards only, and a link reads as its maker, whose roles keep it within the customer. A single company never sees anything about customers.

**Devices.** The page uses what a 2019 browser has: CSS grid, `vmin` units, SVG, `fetch`, and the Fullscreen API only when it exists. No ResizeObserver (the page measures on load and on window resize), no container queries, no `:has()`, no `aspect-ratio`, no `clamp()`. Next.js 16 builds for Chrome 111 and Safari 16.4 by default, which smart TVs and tablets of a few years ago lack: the build would not run on them at all. The web interface now builds for Chrome and Edge 79, Firefox 78 and Safari 14 (`browserslist` in `web/package.json`): TVs from about 2020 (webOS 6, Tizen 6), any Raspberry Pi with Raspberry Pi OS, and iPads from the Air 2 on. When this was decided, the whole output parsed as ES2019 (es-check), and the bundle was the same size within 2 %. Real TVs and tablets were not available to test on; the page was checked in Chromium at 1920×1080 and 3840×2160.

**API.** With a session: `GET /api/dashboards/{id}/display-links`, `POST /api/display-links` (`name`, `dashboard_ids`, `seconds_per_page`, `expires`: `30d`, `1y` or `never`; the answer holds the `url`, the only time the secret is shown), `DELETE /api/display-links/{id}`. Dashboards gain `can_link`. Without one: `POST /api/display/start` (`{"token"}`; 410 for a link that does not work, 403 when nothing is left to show, 429 over a limit), and with a screen session `GET /api/display` and `POST /api/display/data` (`{"dashboard_id"}`). The dashboard data answer gains `now`. Storage: `display_links`, `display_link_dashboards`, `display_sessions`; sessions are deleted with their link, and expired ones when a screen starts.

Rejected:

- **A long session for a screen account.** A user per screen to create, give roles to and never sign out; its session would open the whole interface to whoever is in the room.
- **The secret in the query string or a header on every request.** A query string lands in proxy and browser logs; sending it every 30 seconds gives more chances to leak it than once an hour.
- **A playlist object.** Above.
- **Links that operators or viewers make.** Above.
- **An image of the dashboard rendered on the server**, for screens with no usable browser. A second renderer to keep in step, and every TV sold in the last years has a browser good enough for the page above.
- **A refresh faster than 30 seconds.** Nothing new to show, and more load.
- **Showing the last numbers with a small "offline" dot.** Read from three metres, a dot is invisible and frozen numbers look live.

## Amendment 2026-11-12: personal dashboards read only what their maker administers

A site's administrator who was also a viewer of another customer, or of everything, could make a link to an "Only me" dashboard naming that customer, and the screen showed its metrics and open alerts to whoever had the URL: the maker was checked only for an administrator grant of any reach, and the screen read everything they could read. A display link is an administrator's decision about what they administer, so a personal dashboard's screen now reads as the maker with only their administrator grants, as an API key reads as its maker narrowed to the key (ADR-0101), and making such a link is refused when its panels reach beyond that. Links to a site's or customer's dashboards are unchanged: their sharing already keeps them within a place the maker administers.

## Consequences

- A screen on a wall shows dashboards for a year without anyone signing in, cycles through several, and says plainly when its data is old.
- A leaked link shows its dashboards as its maker sees them, until it expires or someone turns it off; the dashboard page shows when it was used last.
- The whole web interface now builds for 2019–2020 browsers instead of 2023 ones. A later Next.js that drops these targets would need this ADR revisited.
- Rate limits are per instance and reset on restart, like enrollment's (ADR-0007).
- Not in this change: ready-made dashboards on a link (make a copy, which takes one click), a screen's own choice of time range (it uses the dashboard's), and pages bigger than one screen (the panels shrink to fit; a dashboard of six rows is better split in two and cycled).
