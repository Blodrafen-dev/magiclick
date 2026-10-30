#!/bin/sh
# Build, install, verify. Idempotent.
set -eu

BIN_DIR="${HOME}/.local/bin"
UNIT_DIR="${HOME}/.config/systemd/user"
SRC_DIR="$(cd "$(dirname "$0")" && pwd)"
LEGACY="noclickcopy"

echo "==> building"
cargo build --release --manifest-path "$SRC_DIR/Cargo.toml"

echo "==> retiring a pre-rename install"
# Two daemons reading the same pointer fire every paste twice, and the old unit
# points at a binary this build no longer replaces. Retire it before installing.
if [ -f "$UNIT_DIR/${LEGACY}.service" ]; then
    systemctl --user disable --now "${LEGACY}.service" 2>/dev/null || true
    rm -f "$UNIT_DIR/${LEGACY}.service"
    rm -f "$BIN_DIR/${LEGACY}"
    systemctl --user daemon-reload 2>/dev/null || true
    echo "    stopped, disabled and removed ${LEGACY}.service"
fi

echo "==> installing binary to ${BIN_DIR}"
mkdir -p "$BIN_DIR"
install -m 755 "$SRC_DIR/target/release/magiclick" "$BIN_DIR/magiclick"

echo "==> installing systemd user unit"
mkdir -p "$UNIT_DIR"
install -m 644 "$SRC_DIR/contrib/magiclick.service" "$UNIT_DIR/magiclick.service"

echo "==> config"
NEW_CFG="${HOME}/.config/magiclick/config.toml"
OLD_CFG="${HOME}/.config/${LEGACY}/config.toml"
if [ -f "$NEW_CFG" ]; then
    echo "    keeping existing config"
elif [ -f "$OLD_CFG" ]; then
    # The daemon still reads this path. Writing the observe-mode default here
    # would shadow a config you tuned, because the new path wins when it exists.
    echo "    keeping pre-rename config at ${OLD_CFG}"
else
    mkdir -p "$(dirname "$NEW_CFG")"
    cp "$SRC_DIR/config.example.toml" "$NEW_CFG"
    echo "    wrote ${NEW_CFG} (confirm_action = observe)"
fi

echo "==> omarchy bar widget"
if [ -d "${HOME}/.config/omarchy/plugins" ]; then
    rm -rf "${HOME}/.config/omarchy/plugins/dek.${LEGACY}"
    install -d "${HOME}/.config/omarchy/plugins/dek.magiclick"
    cp "$SRC_DIR/contrib/omarchy-widget/manifest.json" "$SRC_DIR/contrib/omarchy-widget/MagiClick.qml" \
        "${HOME}/.config/omarchy/plugins/dek.magiclick/"
    echo "    installed to ~/.config/omarchy/plugins/dek.magiclick"
    echo "    add to the bar:  omarchy bar put dek.magiclick right"
else
    echo "    skipped (no Omarchy shell found)"
fi

echo "==> verifying"
"$BIN_DIR/magiclick" doctor || true

echo
echo "Enable at login with:"
echo "    systemctl --user daemon-reload"
echo "    systemctl --user enable --now magiclick.service"
