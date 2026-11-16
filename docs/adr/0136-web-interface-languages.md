# ADR-0136: The web interface in English and French

Date: 2026-11-16
Status: accepted

## Context

The web interface speaks English only, and French is the first market. The interface is a static export (ADR-0014) served next to the API, without a Node runtime, middleware or locale routing, and its Content Security Policy allows only its own scripts (ADR-0124). About 25,000 lines of components hold their words inline. Formatters in `lib/format.ts` and `components/ui/time.ts` write English numbers, units and dates. The server keeps no preferences per user.

## Decision

**Our own small catalog, no library.** Messages live in `web/src/i18n/messages/`, one file per area of the interface (`shell`, `common`, `format`, then one per group of pages), each holding the English messages and their French translation side by side. English is the reference: its object is `as const`, so `t("area.name", values)` is typed on its keys and on the `{placeholders}` of its text, and the French object must have exactly the same keys. A message that depends on a number is a `{ one, other }` pair, picked by `Intl.PluralRules`, which French needs because it counts zero as one. `rich()` does the same for sentences holding a link or a strong part. The libraries we looked at bring a runtime, ICU parsing or a build plugin. The interface needs lookup, placeholders and two plural forms, which take about a hundred lines.

**Tests hold the catalogs together.** A test fails when a French message is missing or extra, when placeholders or plural forms differ from the English ones, when French typography is broken (a no-break space before `: ; ? !` and inside `« »`, a curly apostrophe), or when the source names a key the English catalog does not have. The type check catches the same keys earlier. Render tests stay in English: tests run with the English locale unless they choose French.

**Translation happens in the browser.** The static HTML is English. `LocaleProvider` in the root layout reads the language once hydrated, sets `<html lang>` and the language `t` and the formatters use, and draws the page again when the language changes. The page under it remounts, so a change loses unsaved input on the page. A change is rare, and this avoids a hook in every component. Nothing is loaded from elsewhere: every catalog is in the bundle, a few kilobytes per language.

**The person's choice, kept in the browser.** A language select under the theme button in the sidebar stores the choice in `localStorage` (`monitorflow.locale`), like the theme. Without a choice, the browser's first language the interface speaks wins (`fr-CA` is French), and English otherwise. The server keeps no preferences per user, so the choice does not follow the person to another browser. Moving it to the user's record is a follow-up.

**Formatting through Intl.** Numbers, percentages, relative times and month names use `Intl` with the active language: "85,3 %", "1 234,5", "il y a 5 minutes", "04 sept.". The units we write ourselves come from the catalog: "j" for days, "Kio" and "Gio" for binary units. Clock times stay 24-hour, and exact times stay `2026-10-22 14:31:53` in both languages.

**What stays English.** Text the server sends: API error messages, incident reasons, audit entries, notification and integration text, and the names of checks the agents report. Wording the interface builds around them is translated. Translating server text is a follow-up: it needs stable codes in the API for each message. The tab titles and the loading lines of the page files are written at build time, so they stay English too. Making them follow the language means setting them on the client.

**Words.** French follows the glossary's French column (`docs/glossary.md`), uses "vous", and keeps labels as short as the English ones.

## Consequences

- New text in the interface goes into a catalog, in English and French, in the same change. The tests block a key without its French message.
- Pages that still hold inline English show it in both languages until their area is moved to the catalog.
- A third language adds one object per area file and one entry in `LOCALES`. The sidebar select lists it.
- Follow-ups: the language stored with the user once the server keeps preferences; server text in French; tab titles in French; French guides and installer text, which follow the same glossary.
