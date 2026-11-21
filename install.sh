#!/bin/sh
# Noctum installer. Linux x86_64 only.
# Usage: curl -fsSL https://raw.githubusercontent.com/SeanCheatham/Noctum/main/install.sh | sh
#
# Options:
#   --uninstall  Remove the noctum binary. Config and data are left in place.

set -eu

REPO="SeanCheatham/Noctum"
BINARY_NAME="noctum"
UNINSTALL=false

for arg in "$@"; do
    case "$arg" in
        --uninstall)
            UNINSTALL=true
            ;;
    esac
done

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info() {
    printf "${GREEN}$1${NC}\n"
}

warn() {
    printf "${YELLOW}$1${NC}\n"
}

error() {
    printf "${RED}$1${NC}\n" >&2
    exit 1
}

get_latest_version() {
    curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" |
        grep '"tag_name":' |
        sed -E 's/.*"v([^"]+)".*/\1/'
}

require_linux_x86_64() {
    OS_NAME="$(uname -s)"
    ARCH="$(uname -m)"
    if [ "$OS_NAME" != "Linux" ] || [ "$ARCH" != "x86_64" ]; then
        error "Noctum runs on Linux x86_64. This machine is ${OS_NAME} ${ARCH}."
    fi
}

uninstall_noctum() {
    info "Uninstalling Noctum..."
    INSTALL_DIR="/usr/local/bin"
    BINARY_PATH="$INSTALL_DIR/$BINARY_NAME"

    if [ -f "$BINARY_PATH" ]; then
        if rm -f "$BINARY_PATH" 2>/dev/null; then
            info "Binary removed."
        else
            warn "Requesting sudo permission to remove $BINARY_PATH"
            sudo rm -f "$BINARY_PATH"
            info "Binary removed."
        fi
    else
        warn "Binary not found at $BINARY_PATH"
    fi

    echo ""
    info "Noctum uninstalled."
    echo "Config and data were left in place:"
    echo "  Config: ~/.config/noctum/"
    echo "  Data:   ~/.local/share/noctum/"
    echo "Remove them with: rm -rf ~/.config/noctum ~/.local/share/noctum"
}

install_noctum() {
    info "Installing Noctum..."
    require_linux_x86_64

    VERSION=$(get_latest_version)
    if [ -z "$VERSION" ]; then
        error "Could not determine the latest version"
    fi
    info "Latest version: $VERSION"

    TARBALL="${BINARY_NAME}-x86_64-unknown-linux-gnu.tar.gz"
    URL="https://github.com/${REPO}/releases/download/v${VERSION}/${TARBALL}"

    TMP_DIR=$(mktemp -d)
    trap 'rm -rf "$TMP_DIR"' EXIT

    info "Downloading $URL"
    if ! curl -fsSL "$URL" -o "$TMP_DIR/$TARBALL"; then
        error "Download failed. Check that a Linux release exists."
    fi

    info "Extracting..."
    tar -xzf "$TMP_DIR/$TARBALL" -C "$TMP_DIR"

    INSTALL_DIR="/usr/local/bin"
    if [ -w "$INSTALL_DIR" ]; then
        mv "$TMP_DIR/$BINARY_NAME" "$INSTALL_DIR/$BINARY_NAME"
        chmod +x "$INSTALL_DIR/$BINARY_NAME"
    else
        warn "Requesting sudo permission to install to $INSTALL_DIR"
        sudo mv "$TMP_DIR/$BINARY_NAME" "$INSTALL_DIR/$BINARY_NAME"
        sudo chmod +x "$INSTALL_DIR/$BINARY_NAME"
    fi

    info "Noctum $VERSION installed."
    echo ""
    echo "Before the first night:"
    echo "  1. rustup target add x86_64-unknown-linux-musl"
    echo "  2. noctum model pull"
    echo "  3. noctum qemu fetch"
    echo "  4. noctum"
    echo "  5. Open http://127.0.0.1:8420"
    echo ""
    echo "qemu fetch needs a published bundle URL. Until then, build qemu/build-bundle.sh and run noctum qemu install on the tarball."
    echo "Configuration: https://github.com/${REPO}/blob/main/config.example.toml"
}

if [ "$UNINSTALL" = true ]; then
    uninstall_noctum
else
    install_noctum
fi
