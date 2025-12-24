#!/bin/bash
# claude-snatch installer
# Usage: curl -sSL https://raw.githubusercontent.com/claude-snatch/claude-snatch/main/install.sh | bash

set -e

# Configuration
REPO="claude-snatch/claude-snatch"
BINARY_NAME="snatch"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

success() {
    echo -e "${GREEN}[OK]${NC} $1"
}

warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

error() {
    echo -e "${RED}[ERROR]${NC} $1" >&2
    exit 1
}

# Detect OS and architecture
detect_platform() {
    local os arch

    case "$(uname -s)" in
        Linux)  os="linux" ;;
        Darwin) os="darwin" ;;
        MINGW*|MSYS*|CYGWIN*) os="windows" ;;
        *) error "Unsupported operating system: $(uname -s)" ;;
    esac

    case "$(uname -m)" in
        x86_64|amd64) arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        *) error "Unsupported architecture: $(uname -m)" ;;
    esac

    echo "${os}-${arch}"
}

# Check for required tools
check_requirements() {
    local missing=()

    if ! command -v curl &> /dev/null && ! command -v wget &> /dev/null; then
        missing+=("curl or wget")
    fi

    if ! command -v tar &> /dev/null; then
        missing+=("tar")
    fi

    if [[ ${#missing[@]} -gt 0 ]]; then
        error "Missing required tools: ${missing[*]}"
    fi
}

# Download file using curl or wget
download() {
    local url="$1"
    local output="$2"

    if command -v curl &> /dev/null; then
        curl -sSL "$url" -o "$output"
    elif command -v wget &> /dev/null; then
        wget -q "$url" -O "$output"
    fi
}

# Get latest release version
get_latest_version() {
    local version

    version=$(download "https://api.github.com/repos/${REPO}/releases/latest" - 2>/dev/null |
              grep '"tag_name":' |
              sed -E 's/.*"([^"]+)".*/\1/')

    if [[ -z "$version" ]]; then
        error "Failed to fetch latest version. Please check your internet connection."
    fi

    echo "$version"
}

# Install from source using cargo
install_from_source() {
    info "Installing from source using cargo..."

    if ! command -v cargo &> /dev/null; then
        error "Cargo not found. Please install Rust: https://rustup.rs"
    fi

    cargo install --git "https://github.com/${REPO}.git" --bin snatch

    success "Installed claude-snatch from source"
    echo
    echo "Run 'snatch --help' to get started"
}

# Install from pre-built binary
install_from_binary() {
    local platform version download_url temp_dir

    platform=$(detect_platform)
    info "Detected platform: ${platform}"

    # Try to get latest release
    version=$(get_latest_version 2>/dev/null || echo "")

    if [[ -z "$version" ]]; then
        warn "No pre-built binaries available yet. Falling back to source install."
        install_from_source
        return
    fi

    download_url="https://github.com/${REPO}/releases/download/${version}/${BINARY_NAME}-${version}-${platform}.tar.gz"

    info "Downloading claude-snatch ${version}..."

    temp_dir=$(mktemp -d)
    trap "rm -rf ${temp_dir}" EXIT

    if ! download "$download_url" "${temp_dir}/archive.tar.gz" 2>/dev/null; then
        warn "Pre-built binary not available for ${platform}. Falling back to source install."
        install_from_source
        return
    fi

    info "Extracting..."
    tar -xzf "${temp_dir}/archive.tar.gz" -C "${temp_dir}"

    info "Installing to ${INSTALL_DIR}..."
    mkdir -p "${INSTALL_DIR}"
    mv "${temp_dir}/${BINARY_NAME}" "${INSTALL_DIR}/"
    chmod +x "${INSTALL_DIR}/${BINARY_NAME}"

    success "Installed claude-snatch ${version} to ${INSTALL_DIR}/${BINARY_NAME}"

    # Check if install directory is in PATH
    if [[ ":$PATH:" != *":${INSTALL_DIR}:"* ]]; then
        warn "${INSTALL_DIR} is not in your PATH"
        echo
        echo "Add the following to your shell profile (~/.bashrc, ~/.zshrc, etc.):"
        echo "  export PATH=\"\$PATH:${INSTALL_DIR}\""
        echo
    fi

    echo
    echo "Run 'snatch --help' to get started"
}

# Main installation logic
main() {
    echo
    echo "  claude-snatch installer"
    echo "  ========================"
    echo

    check_requirements

    # Parse arguments
    local install_method="auto"
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --source)
                install_method="source"
                shift
                ;;
            --binary)
                install_method="binary"
                shift
                ;;
            --dir)
                INSTALL_DIR="$2"
                shift 2
                ;;
            --help|-h)
                echo "Usage: $0 [OPTIONS]"
                echo
                echo "Options:"
                echo "  --source      Install from source using cargo"
                echo "  --binary      Install pre-built binary (default)"
                echo "  --dir DIR     Installation directory (default: ~/.local/bin)"
                echo "  --help        Show this help message"
                exit 0
                ;;
            *)
                error "Unknown option: $1"
                ;;
        esac
    done

    case "$install_method" in
        source)
            install_from_source
            ;;
        binary|auto)
            install_from_binary
            ;;
    esac

    success "Installation complete!"
}

main "$@"
