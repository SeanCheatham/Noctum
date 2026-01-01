#!/bin/sh
# Noctum installer script
# Usage: curl -fsSL https://raw.githubusercontent.com/SeanCheatham/Noctum/main/install.sh | sh
#
# Options:
#   --uninstall  Remove Noctum

set -e

REPO="SeanCheatham/Noctum"
BINARY_NAME="noctum"
UNINSTALL=false

# Parse arguments
for arg in "$@"; do
    case "$arg" in
        --uninstall)
            UNINSTALL=true
            ;;
    esac
done

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

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

# Get the latest version from GitHub API
get_latest_version() {
    curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | \
        grep '"tag_name":' | \
        sed -E 's/.*"v([^"]+)".*/\1/'
}

# Detect OS and architecture
detect_platform() {
    OS_NAME="$(uname -s)"
    ARCH="$(uname -m)"

    case "$OS_NAME" in
        Linux)
            OS="unknown-linux-gnu"
            ;;
        Darwin)
            OS="apple-darwin"
            ;;
        *)
            error "Unsupported operating system: $OS_NAME"
            ;;
    esac

    case "$ARCH" in
        x86_64)
            ARCH="x86_64"
            ;;
        arm64|aarch64)
            ARCH="aarch64"
            ;;
        *)
            error "Unsupported architecture: $ARCH"
            ;;
    esac

    PLATFORM="${ARCH}-${OS}"
}

# Uninstall Noctum
uninstall_noctum() {
    info "Uninstalling Noctum..."

    # Remove binary
    INSTALL_DIR="/usr/local/bin"
    BINARY_PATH="$INSTALL_DIR/$BINARY_NAME"

    if [ -f "$BINARY_PATH" ]; then
        info "Removing binary..."
        # Try without sudo first, fall back to sudo if it fails
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
    info "Noctum uninstalled successfully!"
    echo ""
    warn "Note: Configuration and data files were not removed."
    echo "  Config: ~/.config/noctum/"
    echo "  Data:   ~/.local/share/noctum/"
    echo ""
    echo "To remove all data, run:"
    echo "  rm -rf ~/.config/noctum ~/.local/share/noctum"
}

# Download and install
install_noctum() {
    info "Installing Noctum..."

    # Get latest version
    VERSION=$(get_latest_version)
    if [ -z "$VERSION" ]; then
        error "Could not determine latest version"
    fi

    info "Latest version: $VERSION"

    # Detect platform
    detect_platform
    info "Platform: $PLATFORM"

    # Construct download URL
    TARBALL="${BINARY_NAME}-${PLATFORM}.tar.gz"
    URL="https://github.com/${REPO}/releases/download/v${VERSION}/${TARBALL}"

    # Create temporary directory
    TMP_DIR=$(mktemp -d)
    trap 'rm -rf "$TMP_DIR"' EXIT

    # Download
    info "Downloading from $URL"
    if ! curl -fsSL "$URL" -o "$TMP_DIR/$TARBALL"; then
        error "Download failed. Check that the release exists for your platform."
    fi

    # Extract
    info "Extracting..."
    tar -xzf "$TMP_DIR/$TARBALL" -C "$TMP_DIR"

    # Install to /usr/local/bin (may require sudo)
    INSTALL_DIR="/usr/local/bin"

    if [ -w "$INSTALL_DIR" ]; then
        mv "$TMP_DIR/$BINARY_NAME" "$INSTALL_DIR/$BINARY_NAME"
        chmod +x "$INSTALL_DIR/$BINARY_NAME"
    else
        warn "Requesting sudo permission to install to $INSTALL_DIR"
        sudo mv "$TMP_DIR/$BINARY_NAME" "$INSTALL_DIR/$BINARY_NAME"
        sudo chmod +x "$INSTALL_DIR/$BINARY_NAME"
    fi

    info "Noctum $VERSION installed successfully!"

    echo ""
    info "Next steps:"
    echo "  1. Install Ollama: https://ollama.com/"
    echo "  2. Pull a model: ollama pull qwen2.5-coder"
    echo "  3. Start Noctum: noctum start"
    echo "  4. Open dashboard: http://localhost:8420"
    echo ""
    info "For configuration options, see:"
    echo "  https://github.com/${REPO}/blob/main/config.example.toml"
}

# Main
if [ "$UNINSTALL" = true ]; then
    uninstall_noctum
else
    install_noctum
fi
