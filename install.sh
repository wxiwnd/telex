#!/usr/bin/env sh
set -eu

repo="${TELEX_REPO:-wxiwnd/telex}"
version="${TELEX_VERSION:-latest}"

usage() {
    cat <<'USAGE'
Install tlx from GitHub releases.

Usage:
  curl -fsSL https://raw.githubusercontent.com/wxiwnd/telex/main/install.sh | sh
  curl -fsSL .../install.sh | sh -s -- --version 0.1.0

Environment:
  TELEX_VERSION       Version to install, e.g. 0.1.0 or v0.1.0. Default: latest.
  TELEX_INSTALL_DIR   Install directory. Default: ~/.local/bin.
  TELEX_REPO          GitHub repository. Default: wxiwnd/telex.
USAGE
}

fail() {
    printf 'tlx: %s\n' "$*" >&2
    exit 1
}

info() {
    printf 'tlx: %s\n' "$*"
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --version)
            [ "$#" -ge 2 ] || fail "--version needs a value"
            version="$2"
            shift 2
            ;;
        --version=*)
            version="${1#--version=}"
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            fail "unknown argument: $1"
            ;;
    esac
done

case "$(uname -s)" in
    Linux)
        case "$(uname -m)" in
            x86_64|amd64) target="x86_64-unknown-linux-musl" ;;
            aarch64|arm64) target="aarch64-unknown-linux-musl" ;;
            *) fail "unsupported Linux architecture: $(uname -m)" ;;
        esac
        ;;
    Darwin)
        case "$(uname -m)" in
            arm64|aarch64) target="aarch64-apple-darwin" ;;
            x86_64|amd64) target="x86_64-apple-darwin" ;;
            *) fail "unsupported macOS architecture: $(uname -m)" ;;
        esac
        ;;
    *)
        fail "unsupported operating system: $(uname -s)"
        ;;
esac

if [ "$version" = "latest" ]; then
    base="https://github.com/$repo/releases/latest/download"
else
    case "$version" in
        v*) tag="$version" ;;
        *) tag="v$version" ;;
    esac
    base="https://github.com/$repo/releases/download/$tag"
fi

asset="tlx-$target"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/telex-install.XXXXXX")
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

download() {
    url="$1"
    out="$2"
    if command -v curl >/dev/null 2>&1; then
        curl --proto '=https' --tlsv1.2 -fsSL "$url" -o "$out"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$out" "$url"
    else
        fail "curl or wget is required"
    fi
}

info "downloading $version for $target"
download "$base/$asset" "$tmp/$asset"
download "$base/SHA256SUMS" "$tmp/SHA256SUMS"

expected=$(awk -v name="$asset" '$2 == name {print $1; exit}' "$tmp/SHA256SUMS")
[ -n "$expected" ] || fail "no checksum for $asset"

if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmp/$asset" | awk '{print $1}')
elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')
else
    fail "sha256sum or shasum is required"
fi

[ "$actual" = "$expected" ] || fail "checksum mismatch for $asset"

install_dir="${TELEX_INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$install_dir"
install -m 755 "$tmp/$asset" "$install_dir/tlx"

info "installed tlx to $install_dir/tlx"
"$install_dir/tlx" --version

case ":$PATH:" in
    *":$install_dir:"*) ;;
    *)
        printf '\n' >&2
        printf 'tlx: %s is not in PATH\n' "$install_dir" >&2
        printf 'tlx: add this to your shell profile:\n' >&2
        printf '  export PATH="%s:$PATH"\n' "$install_dir" >&2
        ;;
esac

printf '\nNext:\n  tlx setup\n'
