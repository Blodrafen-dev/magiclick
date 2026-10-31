# Packaging

`magiclick-bin/` is the AUR package. It does not build anything: it downloads
the tarball that `.github/workflows/release.yml` attaches to a `v*` tag.

## What publishes a release

```bash
git tag -a v0.1.0 -m "v0.1.0"
git push origin v0.1.0
```

The workflow builds on `ubuntu-latest` (older glibc than Arch, so the binary runs
on Arch, never the reverse), stages a tarball, and attaches it plus `SHA256SUMS`.

## What has to be true before the AUR entry works

The repositories must be **public**. `makepkg` runs `curl` against the release
URL with no credentials; on a private repo that is a 404 and the package is
unbuildable for everyone but its author.

## Filling in the checksum

`sha256sums_x86_64=('SKIP')` is a placeholder. Once the release exists:

```bash
updpkgsums                     # or paste the value from SHA256SUMS
makepkg --printsrcinfo > .SRCINFO
```

Then, in a clone of `ssh://aur@aur.archlinux.org/magiclick-bin.git`, copy the
two files, commit, push.

## Why there is no udev rule for /dev/input

`/dev/input/event*` is root:input, and granting it broadly would grant
keylogging to every local user. The rule this package ships covers only
`/dev/uinput` (where the paste is injected) and grants it to the active seat via
`uaccess`. Reading the pointer still requires the `input` group, which the user
opts into.
