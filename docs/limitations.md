# Limitations

Written down before anyone asks, because a paste tool that oversells itself is
a paste tool that corrupts a document.

## "Is this a text field?" — not reliably knowable on Linux

The obvious safety check is to verify the click landed in an editable field
before pasting. On Linux there is no dependable way to do that:

- accessibility APIs (AT-SPI) cover GTK/Qt and miss terminals, Electron, and
  canvas-based editors
- Wayland gives a client no hit-testing into another application
- Windows is genuinely better here (`UIA` + `ControlType.Edit`), which is
  ironic and worth saying out loud

So the confirm gesture is doing the work that field detection cannot. You are
the type system.

## Clipboard managers

If another manager owns the selection (CopyQ, GPaste, a custom `wl-paste
--watch` script) it will race. `magiclick doctor` warns when it spots one.
Running two auto-copy daemons at once is not supported and will double-trigger.

## GNOME Wayland

No paste injection. GNOME does not expose a supported path for synthesizing
input; the sanctioned route is libei/EIS, which is not implemented here.
Copy-on-drag works; the router degrades to `observe`.

## Not implemented, deliberately

- **Paste-back** (teleporting the cursor and focus to where you were before
  the drag). It requires *driving* the machine rather than *watching* it, and
  it is the single most fragile thing this program could do. Parked in the
  README roadmap, not promised.
- **Paste-on-focus.** Firing when a window gains focus sounds magical and
  misfires constantly while cycling windows. A click is a decision; focus is
  a side effect.
- **Content-aware routing.** "Only paste URLs into URL bars" is a research
  problem, not a feature. Oversized payloads are refused; that is all.

## Reading every keystroke

The keyboard thread sees all keys, because "any other input disarms" requires
it. Nothing is written to disk, nothing leaves the machine, and the whole path
is visible in `src/drag.rs`. Modifier keys are ignored so that holding shift
mid-drag does not cancel your arm.
