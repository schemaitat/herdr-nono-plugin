#!/bin/sh
# Installs the plugin binary as bin/herdr-nono. It downloads the release that
# matches the version in herdr-plugin.toml and checks its sha256 against the
# release's SHA256SUMS; when there is no such release (an unreleased checkout)
# or no network, it builds from source if cargo is available.
# Usage: scripts/install-binary.sh      (the plugin's [[build]] step runs it)
#
# Environment:
#   HERDR_NONO_RELEASE_URL  base URL of the release assets
#                           (default: the GitHub release of v<version>)
#   HERDR_NONO_RELEASE_DIR  a local directory holding the assets instead of a URL
#   HERDR_NONO_NO_BUILD=1   never fall back to "cargo build"
set -eu

case "$0" in
  */*) script_dir=${0%/*} ;;
  *) script_dir=. ;;
esac
root=$(cd "$script_dir/.." && pwd)
bin_dir="$root/bin"
binary="$bin_dir/herdr-nono"
repo="schemaitat/herdr-nono-plugin"

say() { printf 'herdr-nono-plugin: %s\n' "$*" >&2; }

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/herdr-plugin.toml" | head -n 1)
if [ -z "$version" ]; then
  say "could not read the version from herdr-plugin.toml"
  exit 1
fi

# Already installed and current: nothing to do.
if [ -x "$binary" ] && [ "$("$binary" --version 2>/dev/null || true)" = "herdr-nono $version" ]; then
  say "bin/herdr-nono $version is already installed"
  exit 0
fi

os=$(uname -s)
arch=$(uname -m)
if [ "$os" != "Linux" ]; then
  say "unsupported system $os: the plugin runs on Linux only"
  exit 1
fi
case "$arch" in
  x86_64 | amd64) target="x86_64-unknown-linux-musl" ;;
  aarch64 | arm64) target="aarch64-unknown-linux-musl" ;;
  *)
    say "unsupported architecture $arch: releases exist for x86_64 and aarch64"
    target=""
    ;;
esac

mkdir -p "$bin_dir"
work=$(mktemp -d "${TMPDIR:-/tmp}/herdr-nono-install.XXXXXX")
trap 'rm -rf "$work"' EXIT HUP INT TERM

# Fetches one release asset into $2; fails quietly when it is not there. (Shell
# functions share variables, so the names here must not clash with the caller's.)
fetch() {
  fetch_name=$1
  fetch_dest=$2
  if [ -n "${HERDR_NONO_RELEASE_DIR:-}" ]; then
    cp "$HERDR_NONO_RELEASE_DIR/$fetch_name" "$fetch_dest" 2>/dev/null
    return $?
  fi
  fetch_base=${HERDR_NONO_RELEASE_URL:-https://github.com/$repo/releases/download/v$version}
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 2 --connect-timeout 15 -o "$fetch_dest" "$fetch_base/$fetch_name" 2>/dev/null
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$fetch_dest" "$fetch_base/$fetch_name" 2>/dev/null
  else
    say "neither curl nor wget is available to download $fetch_name"
    return 1
  fi
}

# The sha256 of a file.
checksum() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d ' ' -f 1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d ' ' -f 1
  else
    return 1
  fi
}

# Puts the verified file $1 in place as the plugin binary.
install_file() {
  chmod 755 "$1"
  mv -f "$1" "$binary"
  say "installed bin/herdr-nono $version ($2)"
}

download() {
  [ -n "$target" ] || return 1
  asset="herdr-nono-$version-$target"
  fetch "$asset" "$work/$asset" || { say "no release asset $asset"; return 1; }
  fetch "SHA256SUMS" "$work/SHA256SUMS" || { say "no SHA256SUMS in the release"; return 1; }
  expected=$(sed -n "s/^\([0-9a-f]\{64\}\)  $asset\$/\1/p" "$work/SHA256SUMS" | head -n 1)
  actual=$(checksum "$work/$asset") || { say "neither sha256sum nor shasum is available"; return 1; }
  if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
    say "the checksum of $asset does not match SHA256SUMS; not using it"
    return 1
  fi
  chmod 755 "$work/$asset"
  # It must run here (right architecture, a kernel that supports it).
  if [ "$("$work/$asset" --version 2>/dev/null || true)" != "herdr-nono $version" ]; then
    say "the downloaded $asset does not run on this machine"
    return 1
  fi
  install_file "$work/$asset" "release $target"
}

# A cargo on PATH or in the usual place.
find_cargo() {
  if command -v cargo >/dev/null 2>&1; then
    command -v cargo
  elif [ -x "${HOME:-/nonexistent}/.cargo/bin/cargo" ]; then
    printf '%s\n' "$HOME/.cargo/bin/cargo"
  else
    return 1
  fi
}

build() {
  [ "${HERDR_NONO_NO_BUILD:-0}" != "1" ] || return 1
  cargo=$(find_cargo) || return 1
  say "building from source with $cargo (this takes a minute)"
  (cd "$root" && "$cargo" build --release --locked) >&2 || return 1
  cp "$root/target/release/herdr-nono" "$work/herdr-nono" || return 1
  install_file "$work/herdr-nono" "built from source"
}

if download; then
  exit 0
fi
if build; then
  exit 0
fi
say "could not install the plugin binary."
say "Download herdr-nono-$version-<target> and SHA256SUMS from https://github.com/$repo/releases/tag/v$version,"
say "verify the checksum and save it as $binary (chmod +x), or install Rust (https://rustup.rs) and run: sh scripts/install-binary.sh"
exit 1
