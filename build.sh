#!/usr/bin/env bash
set -e

# Change to the project directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

echo "==> Building track (release mode)..."
cargo build --release

BIN_PATH="$SCRIPT_DIR/target/release/track"
DEST_DIR="${HOME}/.local/bin"

if [ ! -f "$BIN_PATH" ]; then
    echo "Error: Binary not found at $BIN_PATH" >&2
    exit 1
fi

echo "==> Build successful: $BIN_PATH"

# Check for automatic installation flags (-y, --install)
AUTO_INSTALL=false
for arg in "$@"; do
    case "$arg" in
        -y|--yes|--install)
            AUTO_INSTALL=true
            ;;
    esac
done

if [ "$AUTO_INSTALL" = true ]; then
    CHOICE="y"
else
    # If running interactively, prompt user
    if [ -t 0 ]; then
        read -r -p "Do you want to copy 'track' to ${DEST_DIR}? [y/N]: " REPLY
        CHOICE="$REPLY"
    else
        CHOICE="n"
    fi
fi

case "$CHOICE" in
    [yY]|[yY][eE][sS])
        mkdir -p "$DEST_DIR"
        cp -f "$BIN_PATH" "$DEST_DIR/track"
        chmod +x "$DEST_DIR/track"
        echo "==> Successfully copied 'track' to $DEST_DIR/track"
        ;;
    *)
        echo "==> Skipped copy. You can find the executable at $BIN_PATH"
        ;;
esac
