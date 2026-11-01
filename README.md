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
| 1 | Arm on selection — a drag, **or a click that copied** (copy glyphs, "Copy link address"). Re-selecting the pending text does not extend the arm. |
| 2 | Click IS confirm when armed — once it is known to stand alone. A second click inside `multi_click_ms` is a selection, so it neither pastes nor arms. |
| 3 | One shot, then disarm |
| 4 | Anything else disarms (fail closed) |
| 5 | Stale for `arm_timeout_ms` (30s by default), disarm |

## Copying something you did not drag

Half the copying you do is one click: the copy glyph next to a code block, a
`mailto:` chip, "Copy link address". None of that makes a selection, so a
drag-only trigger never sees it.

MagiClick does, without touching the browser. A click with no drag, followed
within `copy_click_window_ms` (600 ms) by a clipboard change **it did not
write**, is a copy you asked for — and it arms exactly like a drag does. The
baselines for both streams are sampled when the button goes down, so a copy you
made earlier (a `Ctrl+C`, a selection the app already had, anything from before
startup) can never be mistaken for one this click caused. Right-click opens the
same window for "Copy link address", with fresh baselines too.

Turn it off with `arm_on_copy = "off"` in your config.

Rule 2 is the one that makes it work: when armed, a single click places the
caret and pastes (one-shot). The paste waits out `multi_click_ms` (300 ms)
first, because a second click inside that window means you are selecting a word —
so a double-click never lands a paste on the text it just highlighted, and never
arms that word either. A stray click after arming still fires,
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
git clone https://github.com/Blodrafen-dev/magiclick
cd magiclick
./install.sh          # builds, installs to ~/.local/bin, installs the user unit
magiclick doctor      # verify — reports what actually works here
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
Left click turns auto-copy on; while it is already on, a click opens the menu
instead, where the arm window and a **Disable** row live. One click to raise the
safety net, two to take it down — a stray click on the bar must not disarm the
thing whose job is to tell you it is armed. Right click opens the
same menu, with the window in force ticked, which is also what the tooltip
reports.

The widget is its own repository, because Omarchy installs a plugin by cloning
one repository and validating its root — and updates it by fast-forwarding that
checkout. A widget copied by a script is not a checkout, so it could never be
updated. Install it from there:

```sh
omarchy plugin add https://github.com/Blodrafen-dev/omarchy-magiclick.git --enable
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

## Support

MagiClick is free, MIT, and asks for nothing. If you want to say thanks, there
are two ways and both are quiet:

```sh
magiclick support          # prints the link
magiclick support --open   # opens it in your browser
```

That link is **[ko-fi.com/magiclick](https://ko-fi.com/magiclick)**, and there is
a **[Buy Me a Coffee page](https://buymeacoffee.com/magiclick)** on the same
handle. The same link is the Support row in the bar widget's panel, and both
pages are listed under the **Sponsor** button on the repository page through
`.github/FUNDING.yml`. GitHub Sponsors is in that file too, and shows up once
the listing is approved.

`support` makes no request of its own — it prints a URL, and reaching the site
is your browser's job. That is deliberate: this daemon reads every key you press
and types into your focused window, so it does not open sockets for anything it
was not asked to do.

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

If this saves you a daily Alt-Tab, `magiclick support` shows where to say thanks.
