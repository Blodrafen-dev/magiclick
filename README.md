# MagiClick

Select text. Click where it goes. It's already there.

`MagiClick` is a small daemon for Linux that turns selecting text into a
clipboard copy, then waits for you to *choose* where to paste it — and never
guesses.

```
select ──► armed ──► click ──► pasted ──► disarmed (click IS confirm)
                                        └─ anything else ──► disarmed
```

## Why another clipboard tool

Because every "smart paste" tool I tried eventually typed the wrong thing into
the wrong window, and none of them could tell me when they were about to.

So this one is built around a single rule: **a paste happens only after a
deliberate confirm gesture, and every other input cancels it.** It is not a
prediction engine. It is a tripwire that is easy to arm and easy to disarm.

## The five rules

| # | Rule |
|---|---|
| 1 | Arm on selection. Re-selecting the pending text does not extend the arm. |
| 2 | Click IS confirm when armed. No separate confirm gesture needed. |
| 3 | One shot, then disarm |
| 4 | Anything else disarms (fail closed) |
| 5 | Stale for `arm_timeout_ms` (30s by default), disarm |

Rule 2 is the one that makes it work: when armed, a single click places the
caret and pastes immediately (one-shot). A stray click after arming still fires,
so the timeout (Rule 5) is your safety net — it disarms before any random click
turns accidental. Right-click the bar glyph for a menu of 1x, 2x, 3x and 4x of
the configured value (30s, 60s, 90s, 120s by default), each labelled with the
seconds it buys. It is session-scoped, so a restart goes back to what your
config says. The same thing from a terminal is `magiclick arm-timeout 2`, or
`magiclick arm-timeout` with no value to step to the next multiple.

Rule 5 is the one that saves you. Without it, a forgotten arm turns your next
random click into a paste.

## Status

Linux, Wayland + X11. Watches the input devices for selections straight from
`/dev/input`, so the compositor never has to cooperate.

| | |
|---|---|
| Tested | Hyprland (wlroots), Arch |
| Should work | sway, i3, X11 generally |
| Copy-only | environments without `/dev/uinput` |
| Planned | Windows, GNOME Wayland |

`magiclick doctor` tells you what actually works on *your* machine rather than
what the README claims. Run it before you trust it.

## Install

```sh
./install.sh          # builds, installs to ~/.local/bin, enables the user unit
magiclick doctor       # verify
```

Requirements: `cargo`, membership in the `input` group, `wl-clipboard` on
Wayland (or `xclip` on X11).

```sh
sudo usermod -aG input "$USER"   # log out and back in
```

## Configure

`~/.config/magiclick/config.toml` — see `config.example.toml`. A pre-rename
`~/.config/noclickcopy/config.toml` is still read while the new
file does not exist, so a config you already tuned keeps working.

The confirm gesture is a key you choose, so it can be a thumb button, a foot
pedal, or nothing at all. On a Logitech MX the side button is bound through
`keyd`:

```ini
# /etc/keyd/default.conf
[ids]
m:0465:b01a

[main]
f13 = macro(M-v)
mouse2 = macro(f13)
```

`confirm_action = "observe"` is the safe default: it logs what *would* have
been pasted and types nothing. Flip to `"paste"` when you trust it.

## Field detection

Click-to-paste is the primary mode: when armed, the click itself pastes. That
makes it safe to fire on a click, because the click is the confirm — but it also
means the click must land in a text field. Linux can tell you that in the
places that matter most, through [AT-SPI](docs/field-detection.md) hit-testing:

| role | verdict |
|---|---|
| `entry`, `text`, `document text`, `paragraph`, `editable text` | paste |
| `button`, `push button`, `toggle button`, `link`, `menu item`, `check box`, `radio button`, `tab`, `slider`, `combo box`, `canvas`, `icon` | refuse |
| `document web`, `section`, `static`, `panel`, anything else | unknown — fail open |

Unknown is a policy decision, not a technical one. **Fail open** keeps every
terminal and every Electron chat composer working; the cost is a click on a
toolbar in an app with no accessibility tree pastes into nothing visible. The
one genuinely dangerous case — a paste landing in a terminal and running a
command — is defused separately: **the trailing newline is stripped from every
selection, always** (`core::trim_selection`, one line ending, `\n` or `\r\n`).
A paste with no newline cannot execute anything.

## Giving it a home in your bar

The bar widget only calls `magiclick`, so nothing platform-specific leaks into
the core. Three states worth showing: **off**, **idle**, **armed**. Seeing
"armed" is the safety feature — you can tell when the gun is loaded.

The widget reads the files in `$XDG_RUNTIME_DIR/magiclick/` (`state`,
`timeout`, `arm_scale`, `arm_base`, `recent`) and runs a handful of commands.
Left click starts watching; while it is already watching, a click opens the
menu instead, where the arm window and a **Stop watching** row live. One click
to raise the safety net, two to take it down — a stray click on the bar must not
disarm the thing whose job is to tell you it is armed. Right click opens the
same menu, with the window in force ticked, which is also what the tooltip
reports.

```sh
./install.sh                                  # copies the widget when Omarchy is present
omarchy bar put dek.magiclick right
```


## When nothing happened

```sh
magiclick log           # the last 15 decisions
magiclick log -n 40
```

Every decision is one line, written where it happens:

```
 just now  a selection in chromium did not change (12 bytes already selected, 12 bytes on the clipboard) — nothing copied
   4s ago  armed 12 bytes from chromium — click where it goes
  41s ago  a click landed 41.2s after the arming, past the 30s window — nothing pasted
```

That is the whole point: a miss is usually one of four things — the selection did
not change, the arm expired, something interrupted it, or the click was refused — and
the log names which. It goes to the journal under the same wording, so
`journalctl --user -u magiclick` and `magiclick log` never disagree. The file is
in `$XDG_RUNTIME_DIR/magiclick/recent` and clears at logout: it is a debugging
aid, not a history you have to curate.

## How it works

Short version: one platform-free state machine (`src/core.rs`, fully unit
tested), plus thin adapters for pointer input, clipboard, and keystroke
injection. Two threads read evdev devices and emit four events. Everything
interesting is in the state machine; everything fragile is in an adapter.

See `docs/how-it-works.md` for the event flow, and `docs/limitations.md` for
the parts that are deliberately unfinished.

## Roadmap

- [x] auto-copy, native evdev
- [x] five-rule router, unit tested
- [x] `doctor`
- [ ] `wl_data_device_manager` natively (drop `wl-clipboard` entirely)
- [ ] app-pair memory: learn "terminal → editor" from your own selections
- [ ] Windows backend (UIA `TextPattern`, not a blind Ctrl+C)
- [ ] GNOME Wayland (libei/EIS)

## License

MIT.

---

If this saves you a daily Alt-Tab, the tip jar is at `<link>`.
