#!/bin/sh
# Noctum installer script
# Usage: curl -fsSL https://raw.githubusercontent.com/SeanCheatham/Noctum/main/install.sh | sh
#
# Options:
#   --service    Install and enable as a background service (systemd/launchd)
#   --no-service Skip service installation (default)
#   --uninstall  Remove Noctum and any installed services

set -e

REPO="SeanCheatham/Noctum"
BINARY_NAME="noctum"
INSTALL_SERVICE=false
UNINSTALL=false

# Parse arguments
for arg in "$@"; do
    case "$arg" in
        --service)
            INSTALL_SERVICE=true
            ;;
        --no-service)
            INSTALL_SERVICE=false
            ;;
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

# Detect OS type
detect_os() {
    OS_NAME="$(uname -s)"

    case "$OS_NAME" in
        Linux)
            OS_TYPE="linux"
            ;;
        Darwin)
            OS_TYPE="macos"
            ;;
        *)
            error "Unsupported operating system: $OS_NAME"
            ;;
    esac
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
            OS_TYPE="linux"
            ;;
        Darwin)
            OS="apple-darwin"
            OS_TYPE="macos"
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

# Uninstall systemd service (Linux)
uninstall_systemd_service() {
    SERVICE_FILE="/etc/systemd/system/noctum.service"

    if [ -f "$SERVICE_FILE" ]; then
        info "Removing systemd service..."
        sudo systemctl stop noctum 2>/dev/null || true
        sudo systemctl disable noctum 2>/dev/null || true
        sudo rm -f "$SERVICE_FILE"
        sudo systemctl daemon-reload
        info "Systemd service removed."
    fi
}

# Uninstall launchd service (macOS)
uninstall_launchd_service() {
    PLIST_FILE="$HOME/Library/LaunchAgents/com.noctum.daemon.plist"

    if [ -f "$PLIST_FILE" ]; then
        info "Removing launchd service..."
        launchctl unload "$PLIST_FILE" 2>/dev/null || true
        rm -f "$PLIST_FILE"
        info "Launchd service removed."
    fi
}

# Uninstall Noctum
uninstall_noctum() {
    info "Uninstalling Noctum..."

    detect_os

    # Remove services first
    case "$OS_TYPE" in
        linux)
            uninstall_systemd_service
            ;;
        macos)
            uninstall_launchd_service
            ;;
    esac

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

# Install systemd service (Linux)
install_systemd_service() {
    info "Installing systemd service..."

    SERVICE_FILE="/etc/systemd/system/noctum.service"

    # Create service file
    sudo tee "$SERVICE_FILE" > /dev/null << 'EOF'
[Unit]
Description=Noctum - AI-powered code analyzer
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/noctum start
Restart=on-failure
RestartSec=10

# Run as current user (will be replaced during install)
User=NOCTUM_USER
Group=NOCTUM_GROUP

# Environment
Environment=HOME=/home/NOCTUM_USER

[Install]
WantedBy=multi-user.target
EOF

    # Replace placeholders with actual user
    CURRENT_USER=$(whoami)
    CURRENT_GROUP=$(id -gn)
    sudo sed -i "s/NOCTUM_USER/$CURRENT_USER/g" "$SERVICE_FILE"
    sudo sed -i "s/NOCTUM_GROUP/$CURRENT_GROUP/g" "$SERVICE_FILE"
    sudo sed -i "s|/home/NOCTUM_USER|$HOME|g" "$SERVICE_FILE"

    # Reload and enable
    sudo systemctl daemon-reload
    sudo systemctl enable noctum
    sudo systemctl start noctum

    info "Systemd service installed and started!"
    echo "  Check status: systemctl status noctum"
    echo "  View logs:    journalctl -u noctum -f"
    echo "  Stop:         sudo systemctl stop noctum"
    echo "  Disable:      sudo systemctl disable noctum"
}

# Install launchd service (macOS)
install_launchd_service() {
    info "Installing launchd service..."

    PLIST_FILE="$HOME/Library/LaunchAgents/com.noctum.daemon.plist"
    mkdir -p "$HOME/Library/LaunchAgents"

    # Create plist file
    cat > "$PLIST_FILE" << EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.noctum.daemon</string>

    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/noctum</string>
        <string>start</string>
    </array>

    <key>RunAtLoad</key>
    <true/>

    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>

    <key>StandardOutPath</key>
    <string>$HOME/.local/share/noctum/noctum.log</string>

    <key>StandardErrorPath</key>
    <string>$HOME/.local/share/noctum/noctum.error.log</string>

    <key>EnvironmentVariables</key>
    <dict>
        <key>HOME</key>
        <string>$HOME</string>
    </dict>
</dict>
</plist>
EOF

    # Create log directory
    mkdir -p "$HOME/.local/share/noctum"

    # Load the service
    launchctl load "$PLIST_FILE"

    info "Launchd service installed and started!"
    echo "  Check status: launchctl list | grep noctum"
    echo "  View logs:    tail -f ~/.local/share/noctum/noctum.log"
    echo "  Stop:         launchctl unload $PLIST_FILE"
    echo "  Remove:       rm $PLIST_FILE"
}

# Install service based on OS
install_service() {
    case "$OS_TYPE" in
        linux)
            if command -v systemctl > /dev/null 2>&1; then
                install_systemd_service
            else
                warn "systemd not found. Skipping service installation."
                warn "You can run 'noctum start' manually or set up your own init script."
            fi
            ;;
        macos)
            install_launchd_service
            ;;
    esac
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

    # Install service if requested
    if [ "$INSTALL_SERVICE" = true ]; then
        echo ""
        install_service
    fi

    echo ""
    info "Next steps:"
    echo "  1. Install Ollama: https://ollama.com/"
    echo "  2. Pull a model: ollama pull qwen2.5-coder"
    if [ "$INSTALL_SERVICE" = true ]; then
        echo "  3. Noctum is running as a background service"
    else
        echo "  3. Start Noctum: noctum start"
        echo "     (or re-run with --service to install as a background daemon)"
    fi
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
