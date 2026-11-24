# ADR-0186: Phones and push notifications

Date: 2026-11-24
Status: accepted; amends ADR-0081 (a phone is a way to reach a person on call)

## Context

The person on call is woken by an email or a chat message (ADR-0081) and then has to sign in to the web interface on a phone, find the alert, acknowledge it (ADR-0082, ADR-0177) and maybe silence it (ADR-0079, ADR-0151). Every one of those steps is slow on a phone at night, and a sign-in that lasts a working day means signing in again at every alert. Phase 18 asks for "a mobile application with push notifications, to acknowledge and silence from a phone".

The usual answer is a native application per store with native push: Apple Push Notification service (APNs) and Firebase Cloud Messaging (FCM). Both need credentials that belong to whoever publishes the application: an APNs key of an Apple developer account and an FCM project of a Google account. MonitorFlow is self-hosted, often on a network without outbound Internet, and nobody but the installation's own administrator holds anything. A native application from the stores would therefore need a push relay run by the project, through which every installation sends its alerts: a service of ours in the path of every alert, which this product has avoided everywhere else.

Browsers have a standard instead. The Push API and the Web Push protocol (RFC 8030) let any web site send notifications to a browser through the push service that browser's maker runs (Apple's, Google's, Mozilla's), with messages encrypted to the browser (RFC 8291) and signed by the site's own key (VAPID, RFC 8292). No account, no relay. Android has had it for years in Chrome and Firefox; iOS and iPadOS have it since 16.4, for a web app added to the home screen.

## Decision

**A web app first, installable on a phone.** The web interface is a Progressive Web App: `manifest.webmanifest` (name, icons with maskable versions, `display: standalone`, start at `/mobile/`), and a service worker, `sw.js`, at the root. The service worker only shows pushed notifications and acts on their buttons; it caches nothing, so the interface is always the one the server serves, and nothing of a former session stays on the phone.

**Pages made for a phone**, under `/mobile/`, outside the desktop shell: the open alerts (`/mobile/`, or what one notification sent together with `?group=`), one alert (`/mobile/incident/?id=`) with Acknowledge and Silence for 30 minutes, 1, 4 or 24 hours (its check, or its whole server; a relay's offline alert silences the relay), who is on call now and next (`/mobile/on-call/`), and this phone (`/mobile/phone/`). Three tabs at the bottom, within reach of a thumb. One column that never scrolls sideways, no scrolling box inside a page, controls one per line or in an even grid, and every control at least 44 px tall (Apple's and WCAG 2.5.5's target size); text fields use 16 px so Safari does not zoom into them. "Full view" goes to the desktop interface, and "Open the full page" to an alert's own page. The installed app opens the phone pages after signing in; the full view stays a tap away.

**A phone is a record of its own**, `phones`: a name, when it was added and last used, and optionally its push subscription (endpoint and keys). A person has at most 10. Adding the phone (`POST /api/me/phones`) is done from the phone, in a signed-in session, which becomes the phone's session:

- **It lasts longer:** 90 days at most, and 30 days without use, or the server's own session times when they are longer (ADR-0069 settings). The person on call should not have to sign in at 3 a.m. Its cookie gets the same lifetime.
- **It stays revocable:** removing the phone (`DELETE /api/me/phones/{id}`, from that phone or another) deletes its sessions (`sessions.phone_id`, `ON DELETE CASCADE`) and its subscription. Disabling the account ends every session and stops notifications at once, and changing one's password ends the other sessions, phones included (ADR-0141). A removed user's phones go with them.
- **Signing in is the web interface's.** The installed app uses the sign-in page, so a sign-in provider (ADR-0145) is offered and required exactly as in a browser: OpenID Connect redirects within the app's window and comes back to it. Nothing of the provider's is stored on the phone besides the session cookie.

**Push is a new way to reach a person, next to their email and chat** (ADR-0081's "How you are reached"). A phone with notifications on reaches its person whenever their contact would: when they are on call, when a follow-up step names them or their schedule (ADR-0082, ADR-0170), and, as an administrator, during a gap. A person whose only way is a phone is not a gap. Each of their phones gets the same groups as their email (ADR-0080), at most 3 notifications per pass, the third saying how many more there are. Channels are not changed: a channel has one fixed destination and status, while push goes to every phone of one person, so it belongs with the person, as ADR-0081 decided for schedules. A team that wants every alert on its phones makes a schedule.

- **The server's own key.** A P-256 key pair is made on first use and stored in `web_push_keys`, its private key sealed like other secrets (ADR-0125, `Column::WebPushKeys`), so every central server of a cluster signs with the same one. Browsers subscribe with its public key (`push_key` of `GET /api/me/phones`).
- **Encryption and signing** are done by the server with `ring`, already used for its other cryptography: `aes128gcm` content encoding with ECDH and HKDF as RFC 8291 says, checked against the RFC's own test vectors, and an ES256 VAPID token for the endpoint's origin, valid 12 hours, whose subject is the web interface's https address. No new dependency and no service of ours.
- **What a notification says is little:** one line of text (the message's first line, cut at 240 characters), the page to open, a tag so a resolution replaces the alert's notification, and the incident's ID. Never a secret and never ADR-0082's acknowledge link, which acts by itself: the push service sees only an encrypted body anyway, but a notification shows on a locked screen.
- **Buttons where the browser has them** (Chrome and Firefox on Android; not iOS today): "Acknowledge", only for an alert the person may acknowledge, posts to `/api/incidents/acknowledge` from the service worker with the phone's session, and the notification then says it was done; without a session it opens the alert's page instead. "Silence…" opens the alert's page with its silence choices. A tap opens the alert, or the list of what was sent together.
- **Delivery.** An alert still open is sent with `Urgency: high`, everything else `normal`; a push service keeps it 4 hours (`TTL`) for a phone that is off. A busy push service (429, 5xx) is tried again like a chat channel. A push service that no longer knows the subscription (404, 410) ends it on the phone's record, which then says to turn notifications on again from the phone. The last success or error shows in the phone's list. A browser that renews its subscription (`pushsubscriptionchange`) hands the new one to its phone from the service worker.
- **Only public push services.** A subscription must be `https` with a host on the Internet, and the server posts through the client limited to public addresses that channels set by non-administrators use (ADR-0078): an endpoint chosen by a browser never makes the server call into its own network.

**The server needs outbound HTTPS to the push services** of the phones' browsers: `web.push.apple.com` (and its subdomains) for iOS and macOS Safari, `fcm.googleapis.com` for Chrome on Android, `updates.push.services.mozilla.com` for Firefox, `*.notify.windows.com` for Edge. The server connects to them directly, not through a proxy, because a proxy would resolve the name itself and defeat the check for public addresses; a firewall must let this traffic out. Without Internet, phones are added, sessions last and the phone pages work on the local network as before, but notifications cannot leave: each attempt records the push service's address as unreachable on the phone, shown in its list and on Send test, while email and chat keep working. Phones themselves reach their push service over their own mobile data, so only the central server's way out matters.

**API.** Under `/api/me/phones`, for the signed-in person only: `GET` (their phones, `push_key`, `session_days`, `idle_days`), `POST` `{name}` (201, this session becomes the phone's; 409 over 10), `PUT /{id}` `{name}`, `DELETE /{id}`, `PUT /{id}/push` with the browser's `PushSubscription.toJSON()` (only from that phone's own session, since the subscription belongs to its browser), `DELETE /{id}/push`, `POST /{id}/test` (counted with the contact's 6 tests a minute). Like the rest of `/api/me/`, v1 leaves them out and an API key gets 403: a phone is the browser it is added from. `GET /api/on-call/contact` adds `phones`, the person's phones with notifications on, and its "Send test" tests them too (`kind: "phone"`). Acknowledging and silencing from a phone use the existing `/api/incidents/acknowledge` and `/api/silences`, which are in v1 and documented.

**Audited:** `user.phone_add`, `user.phone_rename`, `user.phone_push_on` (with the push service's host), `user.phone_push_off` and `user.phone_remove`, besides the acknowledgements and silences themselves, audited as they are from anywhere.

### Native applications, later

Stores are not ruled out, and nothing above is undone by them:

1. **A thin native shell** around the same `/mobile/` pages, with Capacitor (or Tauri's mobile targets), adding what a web app lacks: buttons on iOS notifications, critical alerts that sound through silent mode, and a listing in the stores. The person types the server's address once; the session and the phone record are the ones above.
2. **Native push needs a relay**, since APNs and FCM credentials belong to the publisher. It would be optional and off by default: an installation chooses to send through a relay the project runs, or through its own APNs and FCM credentials for an organisation that publishes its own build. The relay would receive only what a Web Push service receives today: an opaque, encrypted body for a device token, never monitoring data in clear.
3. Until then the roadmap keeps a line for native applications, unticked.

Rejected:

- **Native applications first.** Two stores, two code bases or a framework, publisher accounts, and a relay of ours in the path of every alert, for what the standard already does on both platforms.
- **Push as a kind of notification channel.** A channel sends to one fixed place for a place's events; a phone belongs to a person and should hear what that person is told, no more.
- **A third-party push gateway** (a hosted service that relays Web Push or sends SMS). Another account and another company in the path of alerts.
- **The acknowledge link in the notification.** It would acknowledge for whoever sees the locked screen; the session does it instead.
- **Offline pages cached by the service worker.** An alert list seen offline is out of date by definition, and a cache outlives a removed phone.
- **A session setting for phones.** 90 days and 30 unused, longer only when the server's own times are, and removable at any time; a setting can come if asked for.

## Consequences

- A person on call adds their phone once and is told there, with Acknowledge one tap away on Android and two on iOS, without signing in again for weeks.
- The central server needs outbound HTTPS to push services for notifications; installations without it keep email and chat, and still get the phone pages.
- iOS shows notifications only for the app added to the home screen, and without buttons; the phone page says how to add it.
- The PWA's look and its 44 px targets apply to the phone pages only; the desktop interface is unchanged.
- Native applications remain to be done, with an optional relay.
