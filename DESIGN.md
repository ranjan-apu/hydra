# Hydra - Design System

> Provenance: written from the shipped build (ui/ui.css, ui/index.html, ui/main.js) after the
> second iteration, which replaced the original blue-on-charcoal world with the graphite +
> spring-mint instrument panel the user pinned from the hydra-deepseek build. Tokens below are
> the real values in the stylesheet - if code and this file disagree, the code is right and
> this file needs updating.

## The world

A dark instrument panel for a job that is mostly waiting and watching: night-blue graphite
ground, one spring-mint accent that carries active work, monospace wherever a number or a path
appears. The signature is the **comb**: one vertical bar per connection, filling from the
bottom, so the multi-stream advantage is legible at a glance instead of implied by a percentage.

Surface mode: **Operate**. The user is mid-task; familiarity and precision outrank expression,
and the brand lives in exact details (the comb, the three-stream mark, the probe readout).

## Tokens

Colour is restrained by strategy: neutrals tinted toward the ground, one accent, and three
semantic hues that only ever mean one thing.

| Token | Value | Job |
|---|---|---|
| `--bg` | `#0b0e12` | window ground |
| `--bg-deep` | `#080a0d` | action bar (recedes below) |
| `--panel` | `#12171d` | card surface |
| `--panel-2` | `#171d25` | comb cell |
| `--field` | `#0e1319` | inputs (recessed) |
| `--hairline` | `rgba(255,255,255,.07)` | card + divider border |
| `--hairline-mid` | `rgba(255,255,255,.11)` | field border |
| `--hairline-strong` | `rgba(255,255,255,.17)` | hover border |
| `--text` | `#e8eef4` | primary text, live values |
| `--text-dim` | `#c3cfda` | filenames, labels that read |
| `--muted` | `#94a2b1` | secondary text, hints |
| `--faint` | `#6c7987` | micro labels, placeholders |
| `--accent` | `#4fe0b0` | active work, primary action, live dot |
| `--accent-hi` / `--accent-lo` | `#74f0c6` / `#2fa885` | gradient ends of bars + primary button |
| `--accent-ink` | `#04241b` | text on the mint button |
| `--danger` / `--danger-text` | `#ff6b6b` / `#ffb3b3` | failure |
| `--amber` | `#ffc266` | degraded: retrying cell, range unsupported, folder needed |
| `--info` | `#8fb8ff` | resume context only (the one cool hue) |

**Type**: `-apple-system` for UI, 13px base with a tight ramp (9.5 micro label, 11.5 hint,
12.5 body, 14 wordmark, 15 readout). `ui-monospace` for every datum: URLs, filenames, paths,
byte counts, speeds, percentages, connection labels. Tabular numerals on all counting text.

**Shape**: 8px controls, 12px cards, 100px pills for status and range badges, 7px comb cells.

**Depth**: two-layer soft plates - `0 1px 2px rgba(0,0,0,.34)` plus
`0 6px 18px -12px rgba(0,0,0,.5)` under cards; inset shadows only to recess fields and tracks.
No zero-offset coloured halos.

**Space**: a 4 / 6 / 9 / 13 / 18 rhythm; 14-15px card padding; 20px content gutter; more space
above a section than below its heading.

**Motion**: 140ms for state, 200-220ms for bars, `cubic-bezier(.2,.7,.3,1)`. One authored
moment - the three-stream mark breathes only while bytes are moving.

## Components

- **Titlebar** - brand mark (three bars, staggered heights, mint gradient) + wordmark + one-line
  subtitle; right side holds the transient "Engine ready" badge and the status pill.
- **Status pill** - dot + word, five tones: neutral (idle), mint (ready / busy / live / complete),
  amber (paused), red (failed). The dot pulses only when data is actually flowing.
- **Cards** - every step is a card with a mono step number (01 Source / 02 File details /
  03 Destination). The numbering is the workflow the user actually walks, and step 02 stays
  present in an empty teaching state so the sequence never reads 01 -> 03.
- **URL field** - icon + mono input + inline Probe button; focus draws an accent border plus a
  3px soft ring, never a bare outline.
- **Meta grid** - four labelled cells: Size, Ranges (pill: Supported / Not supported), Probe
  (HEAD / Range GET), Server. Uppercase micro keys, mono values.
- **Resume note** - the only `--info` blue in the app: "Partial file found - 8.09 GiB of
  32.16 GiB done."
- **Comb** - one cell per connection: index, 32px track filling from the bottom, percentage.
  Phase colours: mint (receiving/done), dim (idle/connecting), amber + amber border (retrying).
  Above it, a summary line ("3/8 complete"). A single-stream run shows the sentence
  "single stream" instead of one stretched bar.
- **Buttons** - primary (mint gradient, offset plate), secondary (hairline on translucent
  white), ghost (transparent, red on hover for destructive). Every one has default / hover /
  active / disabled / focus-visible.
- **Done panel** - check, "Download complete - <size>", final path, Reveal in Finder; the full
  bar and the full comb stay on screen above it as the evidence of the run.

## States

`idle -> probing -> ready -> downloading -> paused -> done | error`, plus the single-stream
variant. While a run owns the window (`downloading / paused / error / done`) the setup cards
step aside so the transfer and its controls are the only thing in view; the footer hint reports
what the engine is actually doing, not what was requested. A failure keeps the last measured
frame on screen - percentage, bytes, per-connection bars - because where it died is information.

## Accessibility

- The overall bar is a real `role="progressbar"` with `aria-valuemin/max/now` and an
  `aria-valuetext` ("3.04 GiB of 7.24 GiB, 42 percent") that only updates when the whole
  percent changes, so a 4 Hz stream cannot spam a screen reader.
- `:focus-visible` is a 2px accent outline with 2px offset on every control.
- The connections value is an `<output for="conn-range">` with `aria-valuetext` on the slider.
- The comb is decorative to assistive tech (`aria-hidden`); the summary line carries the fact.
- Text selection is tinted mint; carets take the accent; scrollbars are themed to the ground.
- `prefers-reduced-motion` collapses every animation and transition.

## Named rules

1. **The One Accent Rule** - mint means active work or the primary action, nothing else. Amber
   means degraded, red means failure, and the one blue is resume context.
2. **The Verbatim Rule** - backend strings are shown as sent. The UI never rewrites an error.
3. **The Comb Rule** - the comb is built once per download and mutated per tick; it is never
   re-created per progress event.
4. **The Mono Rule** - if it is a number, a name, or a path, it is monospace and tabular.
5. **The Evidence Rule** - a finished or failed run leaves its numbers and bars on screen.

## Responsive

The window is resizable (880x700 default, 720x600 minimum). At <=720px the meta grid drops to
two columns, the comb to four, the action hint retires, and the content gutter tightens; at
<=520px the stat row drops to two columns and the engine badge retires.
