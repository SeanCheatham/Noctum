#!/bin/sh
# Install Noctum on an Apple silicon Mac.
# Usage: curl -fsSL https://raw.githubusercontent.com/SeanCheatham/Noctum/main/install.sh | sh
#
# Options:
#   --uninstall  Remove both host tools. Keep configuration and data.

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
        Darwin:arm64) TARGET_NAME=aarch64-apple-darwin ;;
        *) error "Noctum does not support ${OS_NAME} ${ARCH}." ;;
    esac
    MACOS_MAJOR=$(sw_vers -productVersion | cut -d. -f1)
    if [ "$MACOS_MAJOR" -lt 26 ]; then
        error "Noctum requires macOS 26 or later."
    fi
}

uninstall_noctum() {
    info "Uninstalling Noctum..."
    INSTALL_DIR="/usr/local/bin"

    for name in noctum noctum-host; do
        if [ -w "$INSTALL_DIR" ]; then
            rm -f "$INSTALL_DIR/$name"
        else
            sudo rm -f "$INSTALL_DIR/$name"
        fi
    done
    info "Noctum was removed. Configuration and data remain in place."
    echo "  ~/Library/Application Support/com.noctum.noctum/"

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
    codesign --verify --strict "$TMP_DIR/noctum-host" || error "The host helper signature is invalid."
    for name in noctum noctum-host; do
        if [ -w "$INSTALL_DIR" ]; then
            mv "$TMP_DIR/$name" "$INSTALL_DIR/$name"
            chmod +x "$INSTALL_DIR/$name"
        else
            sudo mv "$TMP_DIR/$name" "$INSTALL_DIR/$name"
            sudo chmod +x "$INSTALL_DIR/$name"
        fi
    done
    info "Noctum $VERSION is installed."
    echo "Before the first run:"
    echo "  noctum model pull"
    echo "  noctum runtime prepare"
    echo "  noctum"
    echo "Open http://127.0.0.1:8420"
    echo "The first worker build can take several minutes."
    echo "Configuration: https://github.com/${REPO}/blob/main/config.example.toml"
}

if [ "$UNINSTALL" = true ]; then
    uninstall_noctum
else
    install_noctum
fi
