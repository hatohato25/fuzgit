#!/bin/sh
# fuzgit installer for Linux / WSL.
#
#   curl -fsSL https://raw.githubusercontent.com/hatohato25/fuzgit/main/install.sh | sh
#
# Downloads the release tarball that matches this machine, verifies its SHA-256
# checksum, and installs the `gz` binary. macOS is served by Homebrew instead
# (`brew install hatohato25/fuzgit/fuzgit`), so this script refuses to run there.
#
# Everything lives inside main(), which is called on the very last line. A
# truncated download therefore does nothing at all, rather than running half of
# the script — the usual hazard of `curl | sh`.

set -eu

REPO='hatohato25/fuzgit'
# The package is named fuzgit; the binary it installs is `gz` (Cargo.toml [[bin]]).
BINARY='gz'

# Where the binary goes when neither --bin-dir nor FUZGIT_BIN_DIR says otherwise.
#
# `~/.local/bin` is the default because it needs no privileges. Installing into
# /usr/local/bin from a piped script would mean asking for sudo, and a script you
# have not read should not be the thing that asks.
DEFAULT_BIN_DIR="${HOME}/.local/bin"

log() {
    printf '%s\n' "$*" >&2
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

usage() {
    cat >&2 <<'USAGE'
Usage: install.sh [--version <TAG>] [--bin-dir <DIR>]

  --version <TAG>   Release to install (for example v0.5.0). Defaults to the
                    latest release. Also read from FUZGIT_VERSION.
  --bin-dir <DIR>   Directory to install `gz` into. Defaults to ~/.local/bin.
                    Also read from FUZGIT_BIN_DIR.
  -h, --help        Show this message.
USAGE
}

# Fails unless this is Linux. WSL is Linux as far as uname is concerned, so it
# needs no separate branch.
require_linux() {
    os="$(uname -s)"
    case "$os" in
        Linux) ;;
        Darwin)
            die "this script is for Linux / WSL. On macOS use Homebrew: brew install ${REPO}/fuzgit"
            ;;
        *)
            die "unsupported operating system: ${os}. Build from source with: cargo install --path ."
            ;;
    esac
}

# Prints the release target triple for this machine.
#
# Only x86_64 is published for Linux today. Rather than guessing a nearby triple,
# an unknown architecture stops with the source-build instructions — a binary for
# the wrong architecture would fail in a way that is much harder to read.
detect_target() {
    arch="$(uname -m)"
    case "$arch" in
        x86_64 | amd64)
            printf 'x86_64-unknown-linux-musl\n'
            ;;
        aarch64 | arm64)
            die "no prebuilt binary is published for ${arch} on Linux yet. Build from source with: cargo install --git https://github.com/${REPO}.git"
            ;;
        *)
            die "unsupported architecture: ${arch}. Build from source with: cargo install --git https://github.com/${REPO}.git"
            ;;
    esac
}

# Prints the name of the first available download command (curl or wget).
detect_downloader() {
    if command -v curl >/dev/null 2>&1; then
        printf 'curl\n'
    elif command -v wget >/dev/null 2>&1; then
        printf 'wget\n'
    else
        die 'neither curl nor wget is available'
    fi
}

# download <url> <destination>
download() {
    case "$DOWNLOADER" in
        curl) curl -fsSL "$1" -o "$2" ;;
        wget) wget -qO "$2" "$1" ;;
    esac
}

# Prints the tag of the latest release.
#
# Parsed from the API response with sed rather than a JSON tool, because jq and
# python are not guaranteed to exist on a minimal Linux image.
latest_version() {
    body="$(mktemp)"
    download "https://api.github.com/repos/${REPO}/releases/latest" "$body" ||
        die 'failed to ask GitHub for the latest release'

    version="$(sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$body" | head -n 1)"
    rm -f "$body"

    [ -n "$version" ] || die 'could not determine the latest release'
    printf '%s\n' "$version"
}

# verify <tarball> <checksum file>
#
# The checksum file is produced by `sha256sum` in CI, so it holds
# "<hash>  <filename>" and both tools below accept it as-is.
verify() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum -c "$2" >/dev/null 2>&1 || die 'checksum mismatch: refusing to install'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 -c "$2" >/dev/null 2>&1 || die 'checksum mismatch: refusing to install'
    else
        die 'neither sha256sum nor shasum is available, so the download cannot be verified'
    fi
}

# Warns when the install directory is not on PATH, and shows how to add it.
#
# A warning rather than an edit: which shell profile to touch is the user's
# decision, and silently rewriting a dotfile from a piped script is not something
# to do on their behalf.
check_path() {
    case ":${PATH}:" in
        *":$1:"*) ;;
        *)
            log ''
            log "warning: ${1} is not on your PATH."
            log 'Add it by appending this to your shell profile (~/.bashrc, ~/.zshrc, ...):'
            log ""
            log "    export PATH=\"${1}:\$PATH\""
            ;;
    esac
}

main() {
    version="${FUZGIT_VERSION:-}"
    bin_dir="${FUZGIT_BIN_DIR:-${DEFAULT_BIN_DIR}}"

    while [ $# -gt 0 ]; do
        case "$1" in
            --version)
                [ $# -ge 2 ] || die '--version needs a tag'
                version="$2"
                shift 2
                ;;
            --bin-dir)
                [ $# -ge 2 ] || die '--bin-dir needs a directory'
                bin_dir="$2"
                shift 2
                ;;
            -h | --help)
                usage
                exit 0
                ;;
            *)
                usage
                die "unknown option: $1"
                ;;
        esac
    done

    require_linux
    target="$(detect_target)"
    DOWNLOADER="$(detect_downloader)"

    [ -n "$version" ] || version="$(latest_version)"

    tarball="fuzgit-${version}-${target}.tar.gz"
    base="https://github.com/${REPO}/releases/download/${version}"

    work="$(mktemp -d)"
    # Runs on every exit path, including the die() ones
    trap 'rm -rf "$work"' EXIT INT TERM

    log "Downloading ${tarball}"
    download "${base}/${tarball}" "${work}/${tarball}" ||
        die "failed to download ${base}/${tarball}"
    download "${base}/${tarball}.sha256" "${work}/${tarball}.sha256" ||
        die "failed to download the checksum for ${tarball}"

    # The checksum file names the tarball without a path, so verify from inside
    # the working directory
    ( cd "$work" && verify "$tarball" "${tarball}.sha256" )
    log 'Checksum verified'

    tar -xzf "${work}/${tarball}" -C "$work" ||
        die 'failed to extract the tarball'
    [ -f "${work}/${BINARY}" ] || die "the tarball did not contain ${BINARY}"

    mkdir -p "$bin_dir" || die "failed to create ${bin_dir}"
    # Install to a temporary name in the destination first, then move it into
    # place. `mv` within one directory is atomic, so a running `gz` is never
    # replaced by a half-written file.
    staged="${bin_dir}/.${BINARY}.$$"
    cp "${work}/${BINARY}" "$staged" || die "failed to write to ${bin_dir}"
    chmod 755 "$staged"
    mv -f "$staged" "${bin_dir}/${BINARY}" || die "failed to install into ${bin_dir}"

    log "Installed ${BINARY} ${version} to ${bin_dir}/${BINARY}"

    # fuzgit drives the real git; without it every subcommand would fail
    command -v git >/dev/null 2>&1 ||
        log 'warning: git was not found on PATH. fuzgit runs git, so install it too.'

    check_path "$bin_dir"
}

main "$@"
