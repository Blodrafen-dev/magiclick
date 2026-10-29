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
install -m 755 "$SRC_DIR/target/release/noclickcopy" "$BIN_DIR/noclickcopy"

echo "==> installing systemd user unit"
mkdir -p "$UNIT_DIR"
install -m 644 "$SRC_DIR/contrib/noclickcopy.service" "$UNIT_DIR/noclickcopy.service"

echo "==> config"
if [ ! -f "${HOME}/.config/noclickcopy/config.toml" ]; then
    mkdir -p "${HOME}/.config/noclickcopy"
    cp "$SRC_DIR/config.example.toml" "${HOME}/.config/noclickcopy/config.toml"
    echo "    wrote ~/.config/noclickcopy/config.toml (confirm_action = observe)"
else
    echo "    keeping existing config"
fi

echo "==> verifying"
"$BIN_DIR/noclickcopy" doctor || true

echo
echo "Enable at login with:"
echo "    systemctl --user daemon-reload"
echo "    systemctl --user enable --now noclickcopy.service"
