#!/bin/sh
set -eu

# velm cross-platform installer for macOS (arm64/x86_64) and Linux (x86_64/arm64).
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Nokky21/velm/main/scripts/install.sh | sh
#   VELM_VERSION=v0.1.0 VELM_INSTALL_DIR="$HOME/.local/bin" ./scripts/install.sh

REPO="Nokky21/velm"
INSTALL_DIR="${VELM_INSTALL_DIR:-$HOME/.local/bin}"
CONFIG_ROOT="${XDG_CONFIG_HOME:-$HOME/.config}/velm"
SHARE_DIR="${CONFIG_ROOT}/share"
VERSION="${VELM_VERSION:-}"

log() {
    printf 'velm: %s\n' "$*"
}

err() {
    printf 'velm: error: %s\n' "$*" >&2
    exit 1
}

need() {
    command -v "$1" >/dev/null 2>&1 || err "required command not found: $1"
}

detect_platform() {
    os_raw="$(uname -s)"
    case "$os_raw" in
        Darwin) OS="macos" ;;
        Linux)  OS="linux" ;;
        *)      err "unsupported operating system: $os_raw (use scripts/install.ps1 on Windows)" ;;
    esac

    arch_raw="$(uname -m)"
    case "$arch_raw" in
        arm64|aarch64) ARCH="arm64" ;;
        x86_64|amd64)  ARCH="x86_64" ;;
        *)             err "unsupported architecture: $arch_raw" ;;
    esac

    TARGET="${OS}-${ARCH}"
}

resolve_version() {
    if [ -n "$VERSION" ]; then
        case "$VERSION" in
            v*) ;;
            *)  VERSION="v${VERSION}" ;;
        esac
        return
    fi

    log "checking latest GitHub release..."
    api_url="https://api.github.com/repos/${REPO}/releases/latest"
    tag="$(curl -fsSL --retry 3 --connect-timeout 10 --max-time 20 "$api_url" \
        | sed -n 's/^[[:space:]]*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
        | head -n 1)"

    if [ -z "$tag" ]; then
        err "no published GitHub release found yet for ${REPO}. Build from source with: cargo install --path . --locked"
    fi
    VERSION="$tag"
}

verify_sha256() {
    archive_file="$1"
    checksum_file="$2"
    expected="$(awk '{print $1}' "$checksum_file")"
    [ -n "$expected" ] || err "empty SHA-256 checksum file"

    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "$archive_file" | awk '{print $1}')"
    elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "$archive_file" | awk '{print $1}')"
    else
        err "neither sha256sum nor shasum is available to verify download integrity"
    fi

    if [ "$expected" != "$actual" ]; then
        err "SHA-256 mismatch! expected=${expected} actual=${actual}"
    fi
}

main() {
    need curl
    need tar
    need awk
    need sed

    while [ $# -gt 0 ]; do
        case "$1" in
            --version)
                [ $# -ge 2 ] || err "--version requires a tag (e.g. v0.1.0)"
                VERSION="$2"
                shift 2
                ;;
            --dir)
                [ $# -ge 2 ] || err "--dir requires a directory path"
                INSTALL_DIR="$2"
                shift 2
                ;;
            -h|--help)
                cat <<EOF
velm installer (macOS & Linux)

Usage:
  install.sh [--version <tag>] [--dir <install-dir>]

Environment variables:
  VELM_VERSION      Release tag to install (default: latest GitHub release)
  VELM_INSTALL_DIR  Destination directory for binaries (default: \$HOME/.local/bin)
EOF
                exit 0
                ;;
            *)
                err "unknown option: $1"
                ;;
        esac
    done

    detect_platform
    resolve_version

    archive_name="velm-${VERSION}-${TARGET}.tar.gz"
    base_url="https://github.com/${REPO}/releases/download/${VERSION}"
    tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/velm-install.XXXXXX")"
    trap 'rm -rf "$tmp_dir"' EXIT INT TERM

    log "downloading ${archive_name} (${TARGET})..."
    curl -fsSL --retry 3 --connect-timeout 15 -o "${tmp_dir}/${archive_name}" "${base_url}/${archive_name}" \
        || err "failed to download ${base_url}/${archive_name}"
    curl -fsSL --retry 3 --connect-timeout 15 -o "${tmp_dir}/${archive_name}.sha256" "${base_url}/${archive_name}.sha256" \
        || err "failed to download ${base_url}/${archive_name}.sha256"

    log "verifying SHA-256 checksum..."
    verify_sha256 "${tmp_dir}/${archive_name}" "${tmp_dir}/${archive_name}.sha256"

    tar -xzf "${tmp_dir}/${archive_name}" -C "$tmp_dir"
    extracted="${tmp_dir}/velm-${VERSION}-${TARGET}"
    [ -d "$extracted" ] || err "unexpected archive structure"

    mkdir -p "$INSTALL_DIR" "$SHARE_DIR"
    for bin in velm term-ai term-repos term-sql term-claude; do
        if [ -f "${extracted}/${bin}" ]; then
            install -m 755 "${extracted}/${bin}" "${INSTALL_DIR}/${bin}"
        fi
    done

    if [ -f "${extracted}/shell/term.zsh" ]; then
        cp "${extracted}/shell/term.zsh" "${SHARE_DIR}/term.zsh"
    fi
    if [ -f "${extracted}/extras/alacritty.toml" ]; then
        cp "${extracted}/extras/alacritty.toml" "${SHARE_DIR}/alacritty.toml"
    fi

    log "installed velm ${VERSION} into ${INSTALL_DIR}"
    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) ;;
        *)
            printf '\nAdd %s to your PATH (e.g. in ~/.zshrc or ~/.bashrc):\n' "$INSTALL_DIR"
            printf '  export PATH="%s:$PATH"\n' "$INSTALL_DIR"
            ;;
    esac

    printf '\nNext steps:\n'
    printf '  1. Generate base config:  mkdir -p "%s" && velm --default-config > "%s/config.toml"\n' "$CONFIG_ROOT" "$CONFIG_ROOT"
    printf '  2. Enable zsh OSC 133:    echo '\''source "%s/term.zsh"'\'' >> ~/.zshrc\n' "$SHARE_DIR"
    printf '  3. Start velm:            velm\n'
}

main "$@"
