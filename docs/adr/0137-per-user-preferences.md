# ADR-0137: Per-user preferences kept by the server

Date: 2026-11-16
Status: accepted; changes the storage of the language and theme choices of ADR-0136

## Context

The theme and the language of the web interface are kept in the browser's `localStorage` (ADR-0136). A person who signs in from another browser or device starts again from the defaults. The next things people ask to adjust are about themselves too: the main sidebar (labels or icons only, items they never open, their order), and later the page they land on, how dense tables are and their time zone. None of it is a setting of the server: it changes nothing for anyone else.

## Decision

**Per-user against server-wide.** A preference changes only what the signed-in person sees in their own browser: theme, language, sidebar. A setting changes MonitorFlow for everyone or for a site, and stays an administrator's under Settings (ADR-0133). When a choice would change what other people see or receive, like a notification channel, it is a setting, not a preference. The person's on-call contact stays where it is: it decides where their pages go, and its changes are audited.

**Where it is stored.** One JSON document per user in a `user_preferences` table, keyed by the user and removed with them (`ON DELETE CASCADE`). The signed-in person reads it with `GET /api/me/preferences` and replaces it whole with `PUT /api/me/preferences`, under the session and CSRF rules of every other session route. There is no route to read or change another person's preferences, administrators included. The public API leaves `/api/me/` out (ADR-0101), and the handlers refuse an API key: preferences belong to a person in a browser, not to a script.

**A known shape, bounded.** The server validates the document against its schema: `theme` (`system`, `light`, `dark`), `language` (`en`, `fr`), and `sidebar` with `collapsed`, `hidden` and `order`. Every key is optional and means the interface's default when absent. An unknown key or value is refused with 400, which keeps the table from becoming free storage. Sidebar items are named by the web interface's identifiers (`fleet`, `onCall`); the server checks their form only (a letter, then letters, digits or hyphens, 40 at most), at most 64 per list, without duplicates, so a new page needs no server change. A body over 16 KiB is refused, and the table holds the stored document to the same size. A document a later version no longer reads counts as empty instead of failing the page.

**What a role cannot see stays hidden.** The sidebar applies a person's choices to the items their roles show. A preference never shows an item: listing a hidden page in `order` does nothing, and the API still refuses what the roles do not allow. Settings cannot be hidden, so the way back to Preferences always stays. Items are reordered within their section, so the section headings keep their meaning.

**Not audited.** Preference changes write no audit entry. They change nothing beyond the person's own screen, carry no secret and grant nothing; an entry per click on a theme button would bury the events the audit log is for.

**Migration from localStorage.** The web interface keeps `localStorage` as a cache for the first paint: the theme script in `<head>` and the language read before hydration still use it, so a page never flashes the default. After sign-in it loads the document once. When the document has no theme or language and the browser holds one, the browser's choice is written to the server, once; otherwise the server's document wins and refreshes the cache. A change applies at once, is written to the cache at once, and is sent to the server after a short pause, so a burst of clicks makes one request. Two tabs that change preferences at once keep the last one sent.

## Consequences

- Theme, language and sidebar follow the person from one browser to another.
- A new preference adds a field to the schema, in the server and in the web interface, in the same change. Keys are only added: a removed key would make older documents unreadable and reset them.
- Settings becomes visible to everyone, with Preferences as its first page; administrators keep their pages below it.
- Next: the default landing page, table density and the person's time zone, each a new optional key of this document. Server text in the person's language (ADR-0136) can then read `language` from here.
