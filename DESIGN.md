# Hydra - Design System

> Provenance: system summary produced by the ui agent at finish; persisted by lead.
> Tokens are pinned - do not introduce new hues; the accent rule is law.

## Tokens

- **Palette**: bg #151519, card #1d1d23, field #1a1a21, hairlines rgba(255,255,255,.08)/.14,
  text #e9e9ef, muted #9a9aa6, faint #82828c, accent #4f8cff (ink on accent #0b1220),
  success #3ddc84, danger #ff5d5d (banner text tint #ff8080), amber #f5b455.
- **Type**: -apple-system, 13px base (11-18 ramp, 1.125-1.2 steps); ui-monospace for URLs,
  filenames, paths, chips; tabular-nums on all numerals.
- **Shapes**: controls 6px radius, cards 10px, bars 8px on 4px tracks.
- **Depth**: flat tonal layering, no shadows.

## Motion

- 250ms bar width transition; 180ms card-in; pulses only for connecting/retrying phases;
  indeterminate slide for unknown size; prefers-reduced-motion honored.

## Components

Primary (accent + ink), secondary (hairline), danger ghost buttons; stepper; badges with
status dots; chips; the per-connection comb grid.

## Named rules

- **The One Accent Rule**: blue only for active work; green/amber/red carry meaning only.
- **The Verbatim Rule**: backend strings are never reworded.
- **The Comb Rule**: the conn grid is built once per download and mutated per tick -
  never re-created per progress event.
