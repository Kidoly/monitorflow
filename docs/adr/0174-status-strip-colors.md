# ADR-0174: Status strip colors

Date: 2026-11-22
Status: accepted

## Context

ADR-0130 put a strip of counts per state at the top of every page, a count that needs a look tinted and zero and OK left plain, so a large healthy fleet does not shout. In use, the solid fills and contrasting frames of the tinted counts drew the eye in dark mode, the white plain counts and the grey and blue fills clashed in light mode, and on a phone the strip wrapped onto several lines above every page.

## Decision

- Every count above zero, OK included, is a light pane of its state's color: a faint tint and a hairline of the same hue, never a solid fill or a contrasting frame. OK is green. Calm states (silenced and the like) get barely a tint.
- A zero count has no color at all, so it never stands out.
- The count of the state the page shows gets a slightly firmer hairline, nothing that competes with a problem's color.
- On a phone the strip stays one line and scrolls sideways, its scrollbar hidden and a fade at the edge saying more is there.

The rest of ADR-0130 (what is counted, where each count leads) is unchanged.

## Consequences

- A healthy fleet shows a green OK, faint enough that it does not shout; a problem still reads first by its warmer color.
- On a phone some counts sit past the right edge until scrolled to; the fade says so.
