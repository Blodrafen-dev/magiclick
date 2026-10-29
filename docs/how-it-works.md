# How it works

## Event flow

```
/dev/input/event*            src/drag.rs
  pointer thread ──► Drag | Click ─┐
  keyboard thread ─► Confirm | Key ┴─► mpsc ──► src/main.rs
                                                   │
                                                   ▼
                                          src/core.rs  (pure, tested)
                                                   │
                                    Armed | Fire | Disarmed
                                                   │
                              ┌────────────────────┴──────────────┐
                              ▼                                   ▼
                     src/clipboard.rs                      src/inject.rs
                     wl-copy / wl-paste                    /dev/uinput
```

`core.rs` has no platform code and no I/O. It takes an `Event` and returns an
`Outcome`. That is why eleven unit tests cover the entire decision surface of
the program.

## Why evdev instead of the compositor

Neither X11 nor Wayland offers a portable "tell me about drags" API. Wayland
in particular gives an ordinary client no global input access at all, by
design.

`/dev/input` is below the compositor, so the same pointer loop works
everywhere. The cost is permissions: you need the `input` group, and the
program can technically read every key you press — which is why the code is
short and readable on purpose.

## Why the selection check matters

Not every drag is a selection. Window drags, slider scrubs, scrollbar pulls and
file drags all produce the same pointer events.

So rule 1 checks two things: the pointer traveled further than
`drag_threshold`, **and** the PRIMARY selection changed. The second check is
free — the compositor already tells us — and it silently filters out every
drag that was not about text.

## Why clicking is required before confirming

Confirming without a click means the caret is wherever it was before the
drag, which is usually in the document you just selected text from. Pasting
there is a no-op at best and a corrupting edit at worst.

Requiring a click first means the caret was placed deliberately, by the same
hand that is about to confirm.

## Timeouts and the clipboard watchdog

Two independent ways an arm dies, because an arm that outlives the user's
attention is the only real hazard:

- rule 5, a hard timeout
- a watchdog that polls the clipboard while armed and disarms if something
  else took ownership

## v0.2: dropping the subprocesses

`wl-copy` and `wl-paste` are used through a trait (`Clipboard`) that knows
nothing about Wayland. v0.2 implements `wl_data_device_manager` directly via
`wayland-client` and the trait does not change. An owned `wl_data_source` must
stay alive to serve paste requests — that is why `wl-copy` forks and never
exits, and it is the one piece of Wayland protocol work this project owes.
