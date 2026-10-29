#!/bin/sh
# Build, install, verify. Idempotent.
set -eu

BIN_DIR="${HOME}/.local/bin"
UNIT_DIR="${HOME}/.config/systemd/user"
SRC_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "==> building"
cargo build --release --manifest-path "$SRC_DIR/Cargo.toml"

echo "==> installing binary to ${BIN_DIR}"
mkdir -p "$BIN_DIR"
install -m 755 "$SRC_DIR/target/release/dragcopy" "$BIN_DIR/dragcopy"

echo "==> installing systemd user unit"
mkdir -p "$UNIT_DIR"
install -m 644 "$SRC_DIR/contrib/dragcopy.service" "$UNIT_DIR/dragcopy.service"

echo "==> config"
if [ ! -f "${HOME}/.config/dragcopy/config.toml" ]; then
    mkdir -p "${HOME}/.config/dragcopy"
    cp "$SRC_DIR/config.example.toml" "${HOME}/.config/dragcopy/config.toml"
    echo "    wrote ~/.config/dragcopy/config.toml (confirm_action = observe)"
else
    echo "    keeping existing config"
fi

echo "==> verifying"
"$BIN_DIR/dragcopy" doctor || true

echo
echo "Enable at login with:"
echo "    systemctl --user daemon-reload"
echo "    systemctl --user enable --now dragcopy.service"
