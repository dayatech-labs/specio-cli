#!/bin/sh
# Speq installer for macOS and Linux.
#
#   curl --proto '=https' --tlsv1.2 -fsSL https://github.com/dayatech-labs/speq-cli/releases/latest/download/install.sh | sh
#   curl --proto '=https' --tlsv1.2 -fsSL .../install.sh | sh -s -- --version 1.4.0
#
# Installs the `speq` binary into ~/.local/bin without sudo. It downloads SHA256SUMS over HTTPS,
# verifies the binary against it before installing, and never asks for credentials or workspace URLs.
# Authentication happens afterwards, explicitly, with `speq login`.
set -eu

RELEASES="${SPEQ_RELEASE_URL:-https://github.com/dayatech-labs/speq-cli/releases}"
VERSION=""

usage() {
  cat <<USAGE
Usage: install.sh [--version <x.y.z>]

  --version <x.y.z>   install that exact release (default: the latest stable release)
  -h, --help          show this help

Installs to \$HOME/.local/bin. Re-running is safe.
USAGE
}

fail() { printf 'speq installer: %s\n' "$1" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --version) [ $# -ge 2 ] || fail "--version needs a value"; VERSION="${2#v}"; shift 2 ;;
    --version=*) VERSION="${1#--version=}"; VERSION="${VERSION#v}"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; fail "unknown argument: $1" ;;
  esac
done

case "$VERSION" in
  ""|[0-9]*.[0-9]*.[0-9]*) ;;
  *) fail "invalid version: $VERSION" ;;
esac
case "$VERSION" in *[!0-9A-Za-z.+-]*) fail "invalid version: $VERSION" ;; esac

# Only https, except a loopback mirror used by tests.
case "$RELEASES" in
  https://*|http://127.0.0.1*|http://localhost*) ;;
  *) fail "the release URL must use https" ;;
esac

[ -n "${HOME:-}" ] || fail "HOME is not set"
command -v curl >/dev/null 2>&1 || fail "curl is required"

# ---- detect the platform; refuse anything we do not ship rather than guess
os=$(uname -s)
arch=$(uname -m)
case "$os" in
  Darwin)
    if [ "$arch" = "x86_64" ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = "1" ]; then arch=arm64; fi
    case "$arch" in
      arm64|aarch64) target=aarch64-apple-darwin ;;
      x86_64) target=x86_64-apple-darwin ;;
    esac ;;
  Linux)
    case "$arch" in
      x86_64|amd64) target=x86_64-unknown-linux-musl ;;
    esac ;;
esac
if [ -z "${target:-}" ]; then
  fail "unsupported platform: $os $arch. Download an archive for your system from $RELEASES and install it manually."
fi

fetch() { curl --proto '=https,http' --tlsv1.2 -fsSL --retry 2 -o "$2" "$1"; }

if [ -n "$VERSION" ]; then base="$RELEASES/download/v$VERSION"; else base="$RELEASES/latest/download"; fi

bin_dir="$HOME/.local/bin"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/speq-install.XXXXXX")
staged=""
cleanup() { rm -rf "$tmp"; [ -z "$staged" ] || rm -f "$staged"; }
trap cleanup EXIT INT TERM

printf 'Fetching the release manifest...\n'
fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || fail "could not download SHA256SUMS from $base"

# Lines look like "<sha256>  speq-<version>-<target>"; pick ours.
line=$(grep -E "  speq-[0-9][^ ]*-$target\$" "$tmp/SHA256SUMS" | head -n 1 || true)
[ -n "$line" ] || fail "this release has no build for $target"
expected=${line%% *}
name=${line##* }
version=${name#speq-}
version=${version%-"$target"}
case "$expected" in *[!0-9a-f]*) fail "malformed checksum line" ;; esac
[ "${#expected}" -eq 64 ] || fail "malformed checksum line"
[ -z "$VERSION" ] || [ "$version" = "$VERSION" ] || fail "release mismatch: wanted $VERSION, found $version"

printf 'Downloading speq %s for %s...\n' "$version" "$target"
fetch "$RELEASES/download/v$version/$name" "$tmp/$name" || fail "download failed"

if command -v sha256sum >/dev/null 2>&1; then actual=$(sha256sum "$tmp/$name" | cut -d ' ' -f 1)
elif command -v shasum >/dev/null 2>&1; then actual=$(shasum -a 256 "$tmp/$name" | cut -d ' ' -f 1)
else fail "sha256sum or shasum is required to verify the download"; fi
[ "$actual" = "$expected" ] || fail "checksum mismatch; nothing was installed"

# Stage next to the destination, then rename: the old binary stays usable until the very end.
mkdir -p "$bin_dir"
staged="$bin_dir/.speq-install.$$"
cp "$tmp/$name" "$staged"
chmod 755 "$staged"
"$staged" --version >/dev/null 2>&1 || fail "the downloaded binary does not run on this system; nothing was installed"
mv -f "$staged" "$bin_dir/speq"
staged=""
printf 'Installed speq %s to %s/speq\n' "$version" "$bin_dir"

# ---- PATH: add it for future shells only if it is missing, without duplicating the line
case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *)
    case "${SHELL:-}" in
      */zsh) rc="$HOME/.zshrc" ;;
      */bash) if [ "$os" = Darwin ]; then rc="$HOME/.bash_profile"; else rc="$HOME/.bashrc"; fi ;;
      *) rc="$HOME/.profile" ;;
    esac
    if [ -f "$rc" ] && grep -qF "# added by the speq installer" "$rc"; then
      :
    else
      printf '\n# added by the speq installer\nexport PATH="$HOME/.local/bin:$PATH"\n' >> "$rc"
    fi
    printf '\n%s is not on your PATH yet. It was added to %s.\nReload your shell (or run: export PATH="$HOME/.local/bin:$PATH") and then run: speq login\n' "$bin_dir" "$rc"
    exit 0 ;;
esac
printf 'Next: run `speq login`.\n'
