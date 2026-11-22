#!/bin/sh
# Noctum installer for macOS and Linux x86_64.
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

detect_platform() {
    OS_NAME="$(uname -s)"
    ARCH="$(uname -m)"
    case "$OS_NAME:$ARCH" in
        Linux:x86_64) TARGET_NAME=x86_64-unknown-linux-gnu ;;
        Darwin:arm64) TARGET_NAME=aarch64-apple-darwin ;;
        Darwin:x86_64) TARGET_NAME=x86_64-apple-darwin ;;
        *) error "Noctum does not support ${OS_NAME} ${ARCH}." ;;
    esac
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
    if [ "$(uname -s)" = Darwin ]; then
        echo "  ~/Library/Application Support/com.noctum.noctum/"
    else
        echo "  Config: ~/.config/noctum/"
        echo "  Data:   ~/.local/share/noctum/"
    fi
}

install_noctum() {
    info "Installing Noctum..."
    detect_platform

    VERSION=$(get_latest_version)
    if [ -z "$VERSION" ]; then
        error "Could not determine the latest version"
    fi
    info "Latest version: $VERSION"

    TARBALL="${BINARY_NAME}-${TARGET_NAME}.tar.gz"
    URL="https://github.com/${REPO}/releases/download/v${VERSION}/${TARBALL}"

    TMP_DIR=$(mktemp -d)
    trap 'rm -rf "$TMP_DIR"' EXIT

    info "Downloading $URL"
    if ! curl -fsSL "$URL" -o "$TMP_DIR/$TARBALL"; then
        error "Download failed. Check that a release exists for this platform."
    fi

    EXPECTED=$(curl -fsSL "$URL.sha256" | awk '{print $1}')
    if [ "${#EXPECTED}" -ne 64 ]; then
        error "The release checksum is missing."
    fi
    if command -v shasum >/dev/null 2>&1; then
        ACTUAL=$(shasum -a 256 "$TMP_DIR/$TARBALL" | awk '{print $1}')
    else
        ACTUAL=$(sha256sum "$TMP_DIR/$TARBALL" | awk '{print $1}')
    fi
    if [ "$ACTUAL" != "$EXPECTED" ]; then
        error "The release checksum does not match."
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
    if [ "$OS_NAME" = Darwin ]; then
        echo "  1. Start Docker Desktop or another Docker Linux engine."
        echo "  2. noctum model pull"
        echo "  3. noctum runtime prepare"
    else
        echo "  1. rustup target add x86_64-unknown-linux-musl"
        echo "  2. noctum model pull"
        echo "  3. noctum qemu fetch"
    fi
    echo "  4. noctum"
    echo "  5. Open http://127.0.0.1:8420"
    echo ""
    if [ "$OS_NAME" = Darwin ]; then
        echo "The first runtime setup builds a cached Linux worker. It can take several minutes."
    else
        echo "qemu fetch needs a published bundle URL. Until then, build qemu/build-bundle.sh and run noctum qemu install on the tarball."
    fi
    echo "Configuration: https://github.com/${REPO}/blob/main/config.example.toml"
}

if [ "$UNINSTALL" = true ]; then
    uninstall_noctum
else
    install_noctum
fi
