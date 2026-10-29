# Can Linux tell you that you clicked a text field?

Short answer: **sometimes, and reliably in the places that matter most** — but
not through any single mechanism. This document records what was actually
tested on Hyprland 0.56.2 / Wayland in October 2026, so nobody has to guess
again.

The question is the whole product. Copy-on-drag already exists on most desktops
(primary selection). The reason to install NoClickCopy is the second half: it
should paste *where you meant*, without asking. That means answering "is the
thing under the pointer a text field?" at the moment of the click.

## What works

### 1. AT-SPI hit-testing — the real answer

AT-SPI is the accessibility layer. Every participating app publishes a tree of
widgets, each with a **role** (`entry`, `button`, `link`, `document text`, …)
and its geometry. Two calls matter:

- `Accessible.GetChildren` — walk to the frame
- `Component.GetAccessibleAtPoint(x, y, WINDOW)` — ask "what is at this point?"

The second one is a precise, per-widget answer. Measured on a test page with a
button, an input, a bare `div`, a `textarea`, a `select`, and a `canvas`:

| what was under the point | role returned |
|---|---|
| `<input>` | `entry` — `"text field"` |
| `<textarea>` | `entry` |
| `<button>` | `button` |
| `<select>` | `combo box` |
| `<canvas>` | `canvas` |
| plain `<div>` | `section` |
| empty toolbar / chrome | `document web` |
| LibreOffice Writer body | `document text` |

That is a genuine answer, not a heuristic. It is also cheap: the hit-test call
itself is **0.5 ms** server-side.

### 2. Everything else, and why it was rejected

| idea | verdict |
|---|---|
| **Cursor shape** (`I-beam` = text) | Impossible on Wayland. No protocol exposes the current cursor to a client. X11's `XFixes` trick does not cross over. |
| **Focus events** (`object:state-changed:focused`) | Too coarse. Clicking a `<button>` and clicking a `<input>` both merely report `document web`. Useless on its own. |
| **fcitx5 / `zwp_text_input_v3`** ("a text field just took focus") | Theoretically the perfect signal — protocol level, works for every app. In practice the portal input-context count never moved during a click sweep. Only one context existed for the whole session. Dead end on this stack. |
| **Paste and see if it sticks** | Verifies after the damage. Not a veto. |
| **Screenshot / caret detection** | Possible fallback (two `grim` frames ~45 ms each, compare a region for a blinking caret). Not needed where AT-SPI works, and it cannot distinguish a caret from other blink. Kept in reserve. |
| **Windows UIA equivalent** | Does not exist. There is no compositor-level "what is under the pointer" on Wayland. |

## What it covers, measured

| app | tree exposed? | hit-test works? | caveat |
|---|---|---|---|
| Firefox | yes, out of the box | yes | frame size matches the compositor exactly |
| LibreOffice Writer | yes (1410 nodes) | yes — `document text` | large tree, but hit-test is still one call |
| Chromium | **only with `--force-renderer-accessibility`** | yes, exact roles | without the flag it registers on the bus and exposes 2 nodes |
| foot / terminals | **no** | n/a | terminals do not participate in AT-SPI at all |
| Electron / canvas editors | expected no (untested — none installed) | n/a | falls into "unknown" |

### Turning it on

The whole tree is dark unless the accessibility bus is told to enable itself:

```
org.a11y.Status IsEnabled = true
```

on the `org.a11y.Bus` session service. `gsettings set
org.gnome.desktop.interface toolkit-accessibility true` sets the same thing.
It is a session-wide switch, apps pick it up at startup, and it has a real cost:
every app builds and maintains an accessibility tree it otherwise skips.

That is why this must be **opt-in**, not something the installer flips silently.

## The coordinate problem

Wayland gives clients no global screen coordinates, so AT-SPI reports geometry
relative to the window (`CoordType.WINDOW`). To use it we need the pointer
position *in window space*:

```
pointer_screen   ← hyprctl -j cursorpos           (~10 ms)
window_origin    ← hyprctl -j clients             (~8 ms)
frame_size       ← AT-SPI Component.GetExtents
atspi_point      = (pointer_screen - window_origin) / (window_size / frame_size)
```

That last fraction is the trap. **It is not always 1.** Measured here:

| app | compositor window | AT-SPI frame | scale |
|---|---|---|---|
| Firefox | 746 × 396 | 746 × 396 | 1.0000 |
| Chromium | 746 × 808 | 683 × 739 | 1.0922 |

Cause: `org.gnome.desktop.interface text-scaling-factor = 1.0909`. Chromium
applies it to its UI geometry; Firefox ignores it. There is no queryable
per-app scale, so it has to be derived from the two sizes at call time
(`hyprctl` window size ÷ AT-SPI frame size). It is self-calibrating and it
works, but if a frame reports a zero extent the whole check must bail — not
guess.

## Proposed decision procedure

On a click while armed, with a selection held:

1. Ask `cursorpos`. Ask `clients`. Find the window under the pointer.
2. Find the matching AT-SPI application by name, then its frame.
3. Convert the point, hit-test, read the role.
4. Decide:

| role | verdict |
|---|---|
| `entry`, `text`, `document text`, `paragraph`, `editable text` | **paste** |
| `button`, `push button`, `toggle button`, `link`, `menu item`, `check box`, `radio button`, `tab`, `slider`, `combo box`, `canvas`, `icon` | **refuse** (this is the veto — a provable non-field) |
| `document web`, `section`, `static`, `panel`, anything else | **unknown** |

"Unknown" is the important case, and it is a policy decision, not a technical
one:

- **fail open** — paste. Terminals keep working, contenteditable divs keep
  working, chat composers keep working. Cost: a click on a toolbar in an app
  with no accessibility tree still pastes into nothing visible.
- **fail closed** — do nothing. Safest, but breaks every terminal and every
  Electron app, which is most of the interesting surface.

Recommended: fail open, plus neutralise the one genuinely dangerous case.

## The terminal hazard, and how to defuse it without detection

Pasting into a terminal is the one "unknown" case that can *execute*. If the
selection ends in a newline, a blind paste runs a command. That does not
require field detection to fix — strip the trailing newline from anything
pasted, always. A paste with no newline cannot run anything; the user still
has to press Enter themselves.

With that in place, "unknown → paste" is defensible.

## Cost

| step | measured |
|---|---|
| `hyprctl -j cursorpos` | 10 ms |
| `hyprctl -j clients` | 8 ms |
| AT-SPI hit-test | 0.5 ms |
| full tree walk (avoid; used only for diagnosis) | 79 ms / 180 nodes |

Total ~20 ms after the click. Invisible.

Not measured: the steady-state CPU cost of keeping accessibility trees alive in
every app. Distros ship it off for a reason. Treat the number as "small but not
free" until someone profiles it.

## Portability note

This design is Hyprland-shaped only at step 1. Sway has `swaymsg -t get_tree`,
KWin has scripting, GNOME has nothing usable. AT-SPI itself is the portable
part — it is the same on every Linux desktop. The compositor lookup should sit
behind a small trait so another compositor is an implementation, not a rewrite.

## Reproducing any of this

```
gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus \
  --method org.freedesktop.DBus.Properties.Set org.a11y.Status IsEnabled "<true>"
```

Then, in Python (`gi.repository.Atspi` is present on this box):

```python
gi.require_version("Atspi", "2.0")
from gi.repository import Atspi
frame.get_component_iface().get_accessible_at_point(
    x, y, Atspi.CoordType.WINDOW).get_role_name()
```

Remember to set `IsEnabled` back to `false` when done — it is a live,
session-wide switch.
