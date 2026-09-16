# Composition — how an Eclipse pane is built

`docs/STYLE.md` says what things look like. This says what goes where, and in
what order. It exists because the tokens can be perfect and the result still
look generic: four identical panels stacked down a column is exactly what
"generic" means here, and it is what happens when nobody names the hero.

The reference is `docs/design/eclipse-panes.html` — five finished panes
(Display, Sound, Notifications, Power, Privacy & security). Read the markup,
not just this file; every rule below was derived from those five.

## Pane anatomy

Mandatory order, top to bottom:

1. **Header row.** Pane title (24px semibold) + a one-line subtitle that
   carries at most one accented phrase, and 1–3 pill controls at the right.
   All five references also carry a **two-line mono status chip at the far
   right** of the header — the machine's own reading of the pane's subject
   (`2 displays / compositor 60 fps`, `pipewire 1.0.4 / 48 kHz · 128 frames`,
   `Focus on / until 17:30`, `on battery / discharge 8.4 W`,
   `firewall active / 3 rules · 0 blocked today`). Line one is the state, line
   two is the measurement.
2. **The hero block.** One block, unique to this pane, that is not a list.
   See the catalogue below.
3. **One or two inset list panels.** Hairline-separated rows, mono values at
   the right.

**A pane with no hero is incomplete.** That is the rule this document exists
for. If the pane's content genuinely has no magnitude, no geometry, no
schedule, no history and no status set, then the pane is a settings list and
should say so with a single panel — not four.

## Hero catalogue

Each reference pane is distinguished almost entirely by its hero. Pick a
*shape*, then fill it — do not default to another list.

| Shape | Reference | What it is |
|---|---|---|
| **Spatial canvas** | Display · *Arrangement* | A direct-manipulation picture of a physical layout. `DP-1 3440×1440` and `HDMI-A-1 1920×1080` as draggable rectangles, "drag to reposition" as the affordance line. |
| **Live meter** | Sound · *Master volume* | One big number (`64 %`) plus a running level bar, axis-labelled `-∞ · -12 dB · 0 dB`, captioned "Output level". The value moves while you look at it. |
| **Schedule band** | Notifications · *Focus* | A wide accent-bordered banner stating the mode ("Focus · Deep work"), what it does in one sentence, and the window it applies to (`schedule 09:00–17:30`). |
| **Magnitude + history** | Power · *Charge* | `68 %` / `4 h 12 m left` / `8.4 W draw`, a charge bar annotated at `charge limit 80%`, and beneath it `Draw · last 24 h` as a bar chart with a marked peak (22 W) and a `00:00 / 12:00 / now` axis. |
| **Status grid** | Privacy & security · *Permissions* | A 2×2 of subject cards — camera (2 apps, 1 used today), microphone (in use · Meeting client), location (Off · no providers), screen capture (4 apps, 2 need review). Each cell is a subject, a state, and a count. |

Mapping to the code vocabulary: a magnitude is `big_value`, a history is
`BarChart` (with `Highlight::Current` or `::Peak`), an exclusive choice is
`segmented`, a boolean is `Toggle`. A spatial canvas or a status grid has no
primitive yet — it gets **added to `EclipseDE/crates/eclipse-ui/src/widget/parts.rs`**,
never inlined in an app's `view.rs`.

## Rhythm

- **Consecutive blocks must not share a silhouette.** Two `inset_list`s in a
  row is the failure mode; an `inset_list` after a hero is the point. If a
  pane needs three lists, it needs a different pane.
- **One level of inset.** `inset(inset(..))` is not a thing. There is no guard
  in code; this is the honour system.
- **Density is allowed to spike exactly once**, in the hero. The lists beneath
  it stay at `space::ROW_Y` and do not compete.
- Blocks are separated by `space::BLOCK` (18) and nothing else; the content
  column is `space::PANE_Y` / `space::PANE_X` (26/30).

## The accent ledger

**Before writing a `view`, name in a comment the single value that will be
yellow.** One live yellow per pane. In the references:

- Display → the active mode row (`3440×1440 144 Hz`), and VRR active.
- Sound → the master volume reading and its meter.
- Notifications → the focus banner.
- Power → the current draw bar in the 24 h histogram.
- Privacy → the permissions that need review.

Everything else is white at 1.0 / 0.64 / 0.40, or `NEUTRAL`. The accent marks
*state* — what is currently true and changing. It is never decorative, never a
heading colour, and never on two blocks of the same pane.

If a pane genuinely has two live values, it has two panes.

## Coverage smells

Grep your own view before reporting done:

- A percentage, a rate or a count rendered as body text → should be `big_value`.
- A series over time described in prose → should be `BarChart`.
- An on/off described by two pills → should be `Toggle`.
- Three or more mutually exclusive options as separate rows → should be
  `segmented`.
- More than two `inset_list` calls in one pane → the hero is missing.
- Any hex literal, bare `.size(13)` or `.padding(16)` outside `tokens.rs` →
  a bug; add or use a token.
