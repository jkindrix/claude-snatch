#!/usr/bin/env bash
# claude-snatch installer
# Remote: curl -fsSL https://raw.githubusercontent.com/jkindrix/claude-snatch/main/install.sh | bash
# Checkout: ./install.sh  (builds and installs that checkout)

set -euo pipefail

REPO="jkindrix/claude-snatch"
BINARY_NAME="snatch"
DEFAULT_CARGO_HOME="${CARGO_HOME:-${HOME}/.cargo}"
INSTALL_DIR="${INSTALL_DIR:-${DEFAULT_CARGO_HOME}/bin}"
INSTALL_DIR_EXPLICIT=false
INSTALL_TEMP_DIR=""
DRY_RUN=false

SCRIPT_PATH="${BASH_SOURCE[0]:-}"
SCRIPT_DIR=""
if [[ -n "${SCRIPT_PATH}" && -f "${SCRIPT_PATH}" ]]; then
    SCRIPT_DIR=$(cd -- "$(dirname -- "${SCRIPT_PATH}")" && pwd)
fi

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
NC='\033[0m'

info() { echo -e "${BLUE}[INFO]${NC} $1"; }
success() { echo -e "${GREEN}[OK]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
error() {
    echo -e "${RED}[ERROR]${NC} $1" >&2
    exit 1
}

cleanup() {
    if [[ -n "${INSTALL_TEMP_DIR}" && -d "${INSTALL_TEMP_DIR}" ]]; then
        rm -rf -- "${INSTALL_TEMP_DIR}"
    fi
}
trap cleanup EXIT

is_checkout() {
    [[ -n "${SCRIPT_DIR}" && -f "${SCRIPT_DIR}/Cargo.toml" && -d "${SCRIPT_DIR}/src" ]]
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || error "$1 is required"
}

download_file() {
    local url="$1"
    local output="$2"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 2 "${url}" -o "${output}"
    elif command -v wget >/dev/null 2>&1; then
        wget -q "${url}" -O "${output}"
    else
        error "curl or wget is required"
    fi
}

download_stdout() {
    local url="$1"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 2 "${url}"
    elif command -v wget >/dev/null 2>&1; then
        wget -q "${url}" -O -
    else
        return 1
    fi
}

latest_release() {
    local payload
    if ! payload=$(download_stdout "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null); then
        return 1
    fi
    sed -nE 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p' <<<"${payload}" | head -n 1
}

detect_target() {
    local os arch
    os=$(uname -s)
    arch=$(uname -m)
    case "${os}:${arch}" in
        Linux:x86_64|Linux:amd64) echo "x86_64-unknown-linux-gnu" ;;
        Linux:aarch64|Linux:arm64) echo "aarch64-unknown-linux-gnu" ;;
        Darwin:x86_64|Darwin:amd64) echo "x86_64-apple-darwin" ;;
        Darwin:aarch64|Darwin:arm64) echo "aarch64-apple-darwin" ;;
        MINGW*:x86_64|MSYS*:x86_64|CYGWIN*:x86_64) echo "x86_64-pc-windows-msvc" ;;
        *) error "Unsupported platform: ${os} ${arch}" ;;
    esac
}

report_installed_path() {
    local installed="$1"
    local resolved=""
    if command -v "${BINARY_NAME}" >/dev/null 2>&1; then
        resolved=$(command -v "${BINARY_NAME}")
    fi
    if [[ -n "${resolved}" && "${resolved}" != "${installed}" ]]; then
        warn "${installed} was installed, but your shell currently resolves snatch to ${resolved}"
        warn "Adjust PATH or remove the shadowing binary before restarting an MCP client."
    elif [[ ":${PATH}:" != *":$(dirname -- "${installed}"):"* ]]; then
        warn "$(dirname -- "${installed}") is not in PATH"
    fi
}

cargo_install_root_args() {
    if [[ "${INSTALL_DIR_EXPLICIT}" == true ]]; then
        printf '%s\n' --root "$(dirname -- "${INSTALL_DIR}")"
    fi
}

validate_cargo_install_dir() {
    if [[ "${INSTALL_DIR_EXPLICIT}" == true && "$(basename -- "${INSTALL_DIR}")" != "bin" ]]; then
        error "--dir for source installs must name a bin directory"
    fi
}

install_from_git() {
    require_command cargo
    validate_cargo_install_dir
    info "Installing all features from https://github.com/${REPO}.git ..."
    local root_args=()
    mapfile -t root_args < <(cargo_install_root_args)
    local command=(cargo install --git "https://github.com/${REPO}.git" --bin "${BINARY_NAME}"
        --locked --all-features --force "${root_args[@]}")
    if [[ "${DRY_RUN}" == true ]]; then
        printf 'Would run:'
        printf ' %q' "${command[@]}"
        printf '\n'
        return
    fi
    "${command[@]}"
    local installed="${INSTALL_DIR}/${BINARY_NAME}"
    success "Installed ${installed} from the repository"
    report_installed_path "${installed}"
}

install_from_checkout() {
    is_checkout || error "--local requires install.sh to be run from a repository checkout"
    require_command cargo
    validate_cargo_install_dir
    info "Building and installing the current checkout from ${SCRIPT_DIR} ..."
    local root_args=()
    mapfile -t root_args < <(cargo_install_root_args)
    local command=(cargo install --path "${SCRIPT_DIR}" --bin "${BINARY_NAME}"
        --locked --all-features --force "${root_args[@]}")
    if [[ "${DRY_RUN}" == true ]]; then
        printf 'Would run:'
        printf ' %q' "${command[@]}"
        printf '\n'
        return
    fi
    "${command[@]}"
    local installed="${INSTALL_DIR}/${BINARY_NAME}"
    success "Installed local checkout to ${installed}"
    report_installed_path "${installed}"
    warn "Restart or reconnect any MCP client after replacing a running stdio server."
}

install_release() {
    local version="$1"
    local target archive_name binary_file download_url
    target=$(detect_target)
    binary_file="${BINARY_NAME}"
    archive_name="${BINARY_NAME}-${target}.tar.gz"
    if [[ "${target}" == *windows* ]]; then
        binary_file="${BINARY_NAME}.exe"
        archive_name="${BINARY_NAME}-${target}.zip"
    fi
    download_url="https://github.com/${REPO}/releases/download/${version}/${archive_name}"
    info "Downloading ${download_url} ..."
    INSTALL_TEMP_DIR=$(mktemp -d)
    if ! download_file "${download_url}" "${INSTALL_TEMP_DIR}/${archive_name}" 2>/dev/null; then
        return 1
    fi
    if [[ "${archive_name}" == *.zip ]]; then
        require_command unzip
        unzip -q "${INSTALL_TEMP_DIR}/${archive_name}" -d "${INSTALL_TEMP_DIR}"
    else
        require_command tar
        tar -xzf "${INSTALL_TEMP_DIR}/${archive_name}" -C "${INSTALL_TEMP_DIR}"
    fi
    [[ -f "${INSTALL_TEMP_DIR}/${binary_file}" ]] || \
        error "Release archive did not contain ${binary_file}"
    mkdir -p -- "${INSTALL_DIR}"
    mv -- "${INSTALL_TEMP_DIR}/${binary_file}" "${INSTALL_DIR}/${binary_file}"
    if [[ "${binary_file}" != *.exe ]]; then
        chmod 0755 "${INSTALL_DIR}/${binary_file}"
    fi
    success "Installed ${version} to ${INSTALL_DIR}/${binary_file}"
    report_installed_path "${INSTALL_DIR}/${binary_file}"
}

usage() {
    cat <<EOF
Usage: $0 [OPTIONS]

Options:
  --local       Build and install this repository checkout (default in a checkout)
  --source      Install the current main branch with cargo
  --binary      Require the latest prebuilt GitHub release
  --dir DIR     Install into DIR (default: ${DEFAULT_CARGO_HOME}/bin)
  --dry-run     Print the local/source Cargo command without installing
  --help        Show this help message

Without an option, a checked-out install.sh installs that checkout. A piped
installer prefers a release and falls back to a cargo build when no release
exists. All source builds include the MCP and Codex features and replace an
older binary of the same package version.
EOF
}

main() {
    local install_method="auto"
    if is_checkout; then
        install_method="local"
    fi
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --local) install_method="local"; shift ;;
            --source) install_method="source"; shift ;;
            --binary) install_method="binary"; shift ;;
            --dir)
                [[ $# -ge 2 ]] || error "--dir requires a value"
                INSTALL_DIR="$2"
                INSTALL_DIR_EXPLICIT=true
                shift 2
                ;;
            --dry-run) DRY_RUN=true; shift ;;
            --help|-h) usage; exit 0 ;;
            *) error "Unknown option: $1" ;;
        esac
    done

    echo
    echo "  claude-snatch installer"
    echo "  ========================"
    echo

    case "${install_method}" in
        local) install_from_checkout ;;
        source) install_from_git ;;
        binary)
            local version
            version=$(latest_release) || error "No GitHub release exists; use --source"
            install_release "${version}" || \
                error "No release artifact is available for $(detect_target); use --source"
            ;;
        auto)
            local version=""
            if version=$(latest_release) && [[ -n "${version}" ]]; then
                if ! install_release "${version}"; then
                    warn "No release artifact exists for $(detect_target); falling back to source"
                    install_from_git
                fi
            else
                warn "No GitHub release exists; falling back to a cargo install from main"
                install_from_git
            fi
            ;;
    esac
    if [[ "${DRY_RUN}" == true ]]; then
        success "Dry run complete; nothing was installed"
    else
        success "Installation complete"
    fi
}

main "$@"
