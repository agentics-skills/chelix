#!/bin/sh
# Chelix installer script
# https://github.com/agentics-skills/chelix
#
# Usage:
#   curl -fsSL https://github.com/agentics-skills/chelix/raw/master/install.sh | sh
#
# Or with options:
#   curl -fsSL https://github.com/agentics-skills/chelix/raw/master/install.sh | sh -s -- --version=0.1.3

set -e

GITHUB_REPO="agentics-skills/chelix"
BINARY_NAME="chelix"
TOOLS_SERVICE_NAME="chelix-tools-service"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

# Default options
VERSION=""

# Colors (disabled if not a terminal)
if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[0;33m'
    BLUE='\033[0;34m'
    BOLD='\033[1m'
    NC='\033[0m'
else
    RED=''
    GREEN=''
    YELLOW=''
    BLUE=''
    BOLD=''
    NC=''
fi

info() {
    printf '%b%b %b%s%b\n' "$BLUE==>" "$NC" "$BOLD" "$1" "$NC"
}

success() {
    printf '%b%b %b%s%b\n' "$GREEN==>" "$NC" "$BOLD" "$1" "$NC"
}

warn() {
    printf '%b %s\n' "${YELLOW}Warning:${NC}" "$1" >&2
}

error() {
    printf '%b %s\n' "${RED}Error:${NC}" "$1" >&2
    exit 1
}

# Parse arguments
while [ $# -gt 0 ]; do
    case "$1" in
        --version=*)
            VERSION="${1#*=}"
            ;;
        -h|--help)
            cat <<EOF
Chelix installer

Usage:
    install.sh [OPTIONS]

Options:
    --version=VERSION   Install a specific version (default: latest)
    -h, --help          Show this help message

Environment variables:
    INSTALL_DIR         Binary installation directory (default: ~/.local/bin)

Examples:
    curl -fsSL https://github.com/agentics-skills/chelix/raw/master/install.sh | sh
    curl -fsSL https://github.com/agentics-skills/chelix/raw/master/install.sh | sh -s -- --version=0.1.3
EOF
            exit 0
            ;;
        *)
            warn "Unknown option: $1"
            ;;
    esac
    shift
done

detect_os() {
    OS="$(uname -s)"
    case "$OS" in
        Darwin)
            echo "macos"
            ;;
        Linux)
            echo "linux"
            ;;
        MINGW*|MSYS*|CYGWIN*)
            echo "windows"
            ;;
        *)
            echo "unknown"
            ;;
    esac
}

detect_arch() {
    ARCH="$(uname -m)"
    case "$ARCH" in
        x86_64|amd64)
            echo "x86_64"
            ;;
        aarch64|arm64)
            echo "aarch64"
            ;;
        armv7l)
            echo "armv7"
            ;;
        i386|i686)
            echo "i686"
            ;;
        *)
            echo "$ARCH"
            ;;
    esac
}

command_exists() {
    command -v "$1" >/dev/null 2>&1
}

get_latest_version() {
    # Extract tag_name, stripping optional leading "v" prefix.
    if command_exists curl; then
        curl -fsSL "https://api.github.com/repos/${GITHUB_REPO}/releases/latest" | grep '"tag_name":' | sed -E 's/.*"tag_name": *"v?([^"]+)".*/\1/'
    elif command_exists wget; then
        wget -qO- "https://api.github.com/repos/${GITHUB_REPO}/releases/latest" | grep '"tag_name":' | sed -E 's/.*"tag_name": *"v?([^"]+)".*/\1/'
    else
        error "Neither curl nor wget found. Please install one of them."
    fi
}

# Return the GitHub release tag for a given version.
# Date-based versions (YYYYMMDD.NN) are bare tags; semver gets a "v" prefix.
release_tag() {
    v="$1"
    case "$v" in
        [0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9].*)
            echo "$v" ;;
        *)
            echo "v$v" ;;
    esac
}

download() {
    url="$1"
    dest="$2"
    if command_exists curl; then
        curl -fsSL "$url" -o "$dest"
    elif command_exists wget; then
        wget -q "$url" -O "$dest"
    else
        error "Neither curl nor wget found. Please install one of them."
    fi
}

verify_checksum() {
    file="$1"
    expected_sha256="$2"

    if command_exists sha256sum; then
        actual=$(sha256sum "$file" | cut -d' ' -f1)
    elif command_exists shasum; then
        actual=$(shasum -a 256 "$file" | cut -d' ' -f1)
    else
        warn "Cannot verify checksum (sha256sum/shasum not found)"
        return 0
    fi

    if [ "$actual" != "$expected_sha256" ]; then
        error "Checksum verification failed!\nExpected: $expected_sha256\nActual: $actual"
    fi
}

ensure_install_dir() {
    if [ ! -d "$INSTALL_DIR" ]; then
        mkdir -p "$INSTALL_DIR"
    fi
}

install_shared_assets() {
    source_dir="$1"
    if [ ! -d "$source_dir" ]; then
        return 0
    fi

    share_dir="$HOME/.chelix/share"
    mkdir -p "$share_dir"
    cp -R "$source_dir"/. "$share_dir"/
    info "Installed shared assets to $share_dir"
}

add_to_path_instructions() {
    shell_name=$(basename "$SHELL")
    case "$shell_name" in
        bash)
            rc_file="$HOME/.bashrc"
            ;;
        zsh)
            rc_file="$HOME/.zshrc"
            ;;
        fish)
            rc_file="$HOME/.config/fish/config.fish"
            ;;
        *)
            rc_file="$HOME/.profile"
            ;;
    esac

    # Check if already in PATH
    case ":$PATH:" in
        *":$INSTALL_DIR:"*)
            return
            ;;
    esac

    printf "\n"
    warn "$INSTALL_DIR is not in your PATH."
    printf "Add it by running:\n\n"
    if [ "$shell_name" = "fish" ]; then
        printf "  ${BOLD}fish_add_path %s${NC}\n\n" "$INSTALL_DIR"
    else
        printf "  ${BOLD}echo 'export PATH=\"%s:\$PATH\"' >> %s${NC}\n\n" "$INSTALL_DIR" "$rc_file"
    fi
    printf "Then restart your shell or run:\n"
    printf "  ${BOLD}source %s${NC}\n" "$rc_file"
}

# Installation methods

install_binary() {
    os="$1"
    arch="$2"
    version="$3"

    # Determine target triple
    case "$os" in
        macos)
            target="${arch}-apple-darwin"
            ;;
        linux)
            target="${arch}-unknown-linux-gnu"
            ;;
        *)
            error "Unsupported OS for binary installation: $os"
            ;;
    esac

    tag=$(release_tag "$version")
    tarball="${BINARY_NAME}-${version}-${target}.tar.gz"
    url="https://github.com/${GITHUB_REPO}/releases/download/${tag}/${tarball}"
    checksum_url="${url}.sha256"

    info "Downloading ${BINARY_NAME} v${version} for ${target}..."

    tmpdir=$(mktemp -d)
    trap 'rm -rf "$tmpdir"' EXIT

    download "$url" "$tmpdir/$tarball" || error "Failed to download $tarball. Check if a release exists for your platform."

    # Verify checksum
    if download "$checksum_url" "$tmpdir/checksum.sha256" 2>/dev/null; then
        expected_sha=$(cut -d' ' -f1 "$tmpdir/checksum.sha256")
        verify_checksum "$tmpdir/$tarball" "$expected_sha"
        info "Checksum verified"
    else
        warn "Could not download checksum file, skipping verification"
    fi

    # Extract and install
    tar -xzf "$tmpdir/$tarball" -C "$tmpdir"

    # Validate the complete required payload before replacing any installed binary.
    if [ ! -f "$tmpdir/$BINARY_NAME" ]; then
        error "Release archive is missing required $BINARY_NAME binary"
    fi
    if [ ! -f "$tmpdir/$TOOLS_SERVICE_NAME" ]; then
        error "Release archive is missing required $TOOLS_SERVICE_NAME binary"
    fi
    linux_tools_service=""
    if [ "$os" = "macos" ]; then
        linux_tools_service="$TOOLS_SERVICE_NAME-linux-$arch"
        if [ ! -f "$tmpdir/$linux_tools_service" ]; then
            error "Release archive is missing required Linux sandbox artifact $linux_tools_service"
        fi
    fi

    ensure_install_dir
    mv "$tmpdir/$BINARY_NAME" "$INSTALL_DIR/$BINARY_NAME"
    chmod +x "$INSTALL_DIR/$BINARY_NAME"
    mv "$tmpdir/$TOOLS_SERVICE_NAME" "$INSTALL_DIR/$TOOLS_SERVICE_NAME"
    chmod +x "$INSTALL_DIR/$TOOLS_SERVICE_NAME"

    if [ -n "$linux_tools_service" ]; then
        mv "$tmpdir/$linux_tools_service" "$INSTALL_DIR/$linux_tools_service"
        chmod +x "$INSTALL_DIR/$linux_tools_service"
    fi

    if [ -f "$tmpdir/chelix-embedding-service" ]; then
        mv "$tmpdir/chelix-embedding-service" "$INSTALL_DIR/chelix-embedding-service"
        chmod +x "$INSTALL_DIR/chelix-embedding-service"
    fi

    if [ -d "$tmpdir/share/chelix" ]; then
        install_shared_assets "$tmpdir/share/chelix"
    elif [ -d "$tmpdir/share/web" ] && [ -d "$tmpdir/share/wasm" ]; then
        install_shared_assets "$tmpdir/share"
    fi

    success "Chelix installed to $INSTALL_DIR/$BINARY_NAME"
    add_to_path_instructions
}

# Main installation logic

main() {
    printf "\n"
    printf "  ${BOLD}Chelix Installer${NC}\n"
    printf "  Personal AI gateway - one binary, multiple LLM providers\n"
    printf "\n"

    OS=$(detect_os)
    ARCH=$(detect_arch)

    info "Detected: $OS ($ARCH)"

    if [ "$OS" = "windows" ]; then
        error "Windows is not supported by this installer. Please download the binary manually from:\nhttps://github.com/${GITHUB_REPO}/releases"
    fi

    if [ "$OS" = "unknown" ]; then
        error "Unsupported operating system: $(uname -s)"
    fi

    # Get version
    if [ -z "$VERSION" ]; then
        info "Fetching latest version..."
        VERSION=$(get_latest_version)
        if [ -z "$VERSION" ]; then
            error "Failed to determine latest version"
        fi
    fi
    info "Version: $VERSION"

    install_binary "$OS" "$ARCH" "$VERSION"

    # Verify installation
    if command_exists "$BINARY_NAME"; then
        installed_version=$("$BINARY_NAME" --version 2>/dev/null | head -1 || echo "unknown")
        printf "\n"
        success "Installation complete!"
        printf "  ${BOLD}%s${NC}\n" "$installed_version"
        printf "\n"
        printf "Get started:\n"
        printf "  ${BOLD}chelix${NC}          # Start the gateway\n"
        printf "  ${BOLD}chelix --help${NC}   # Show help\n"
        printf "\n"
        printf "Project: ${BLUE}https://github.com/agentics-skills/chelix${NC}\n"
    elif [ -x "$INSTALL_DIR/$BINARY_NAME" ]; then
        printf "\n"
        success "Installation complete!"
        printf "\n"
        add_to_path_instructions
    fi
}

main
