# Stage 1.0 — renderer measurement

Screen 1c built once in `crates/spike`, launched under both Dioxus 0.7.10 targets.
Optimized builds (`--profile spike`: opt-level 3, no fat LTO), Windows 11, same
machine, same session. `conhost.exe` excluded — it exists only because the spike
is launched from a console.

## Numbers

| | WebView2 (`dioxus/desktop`) | Blitz (`dioxus/native`) |
|---|---|---|
| Resident, idle | **353 MB** | **250 MB** |
| Processes | **7** — ours at 33 MB plus six `msedgewebview2` | **1** |
| Binary | 4.8 MB | 24.6 MB |
| Cold build | ~2 min | ~4 min |

Both numbers are far above the budget written into the plan before measuring
(≤250 MB on WebView2, ≤80 MB on Blitz). Those figures were guesses and are now
replaced by these. For scale, the client that prompted this project sits at
7864 MB, so either renderer is between one and two orders of magnitude better —
the choice between them is not what saves the project from that.

## What Blitz does not draw

Four defects on the first screen ported, not four across the whole set:

1. **Blurred `box-shadow` is not painted.** The power ring loses both its outer
   glow and its inset bloom and renders as a bare 1px circle. Hard inset rings
   (`inset 0 0 0 1px`, used on the rail) do paint, so the gap is the blur, not
   box-shadow as a whole — the status page's plain "supported" is optimistic.
   This is the screen's focal point and the design system's signature.
2. **`text-transform: uppercase` is ignored.** Every section label and kicker in
   the mockups relies on it: `СЕРВЕР`/`ПРИЁМ`/`ОТДАЧА` render as `Сервер`/`Приём`/
   `Отдача`, `REALITY · VISION · TCP 443` keeps its source casing.
3. **Flex columns do not size to their content.** The three stat columns overflow
   and collide: `Нидерланды · 34 мсза сессию 4,82 ГБза сессию 612 МБ`. This is a
   layout bug rather than a missing paint, and it is the one that worries me —
   it means column widths have to be pinned by hand wherever text is dynamic.
4. **Inline whitespace is dropped** between a text node and a nested span:
   `12,4 МБ/с` becomes `12,4МБ/с`.

What did work, and was not obvious: flexbox in general, gaps, border-radius,
letter-spacing, `font-variant-numeric: tabular-nums`, Cyrillic, the tag classes,
and inline SVG with colours as attributes. Icons drawn as SVG with `stroke="…"`
render identically under both, which settles the icon strategy — Blitz does not
style SVG, so `currentColor` and class-driven fills would have silently vanished.

## Decision

**WebView2 for now, Blitz kept as an open door.**

The 100 MB and six processes Blitz saves are real, and on the far side of stage 3
they may be worth taking. Today they buy less than they cost: four defects on the
first screen implies an unknown count across eleven, and the workarounds deform
the design rather than adapting it — pre-uppercased strings, hand-pinned column
widths, and a hero element with no glow. The risk budget for this project belongs
in the TLS and REALITY work, not in the renderer.

Nothing is spent by choosing this way round. The RSX and the CSS are identical
between targets, `crates/spike` keeps building under both, and the switch stays a
feature flag. Re-run this measurement when Blitz reaches 1.0:

```bash
cargo build -p bliss-spike --profile spike
cargo build -p bliss-spike --profile spike --no-default-features --features native
```

## Follow-ups this turned up

- Inter is not bundled. The mockups pull it from Google Fonts, which a desktop
  app must not do at every start; ship the font file and drop the `@import`.
- The window still wears the OS title bar and the name "Dioxus App". The design
  calls for a custom shell with no system chrome.
- `color-mix()` and gradients were not isolated in this pass. The fading rule and
  the divider token both use them, and both renderers drew *something*; whether
  Blitz fades or draws a solid line needs its own look if we ever revisit.
