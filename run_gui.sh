#!/usr/bin/env bash
# Build the standalone Tauri desktop, never a binary that needs a dev server.
set -euo pipefail

usage() {
  cat <<'HELP'
Usage: ./run_gui.sh [--help]

Build and launch Starkbot Neo from this checkout on macOS or Linux (x86-64).
Run from any directory; checkout paths with spaces are supported.

Requires Rust via rustup (rust-toolchain.toml), Node.js 22.12+ and npm.
macOS: install Xcode Command Line Tools with: xcode-select --install
Linux: use a graphical desktop and install the native build packages in README.md.

On first use, installs locked UI dependencies and a pinned, checkout-local
Tauri CLI through npm. Every launch rebuilds the frontend and asks Cargo to
incrementally build the release desktop, so source changes are not missed.
macOS builds an app bundle with microphone metadata; Linux builds a standalone
executable. Both embed the frontend: no Vite server is needed or started.

Downloads/builds can take several minutes and need disk space. No sudo, system
package installation, global npm installation or application installation is
performed. Build failures stop launch; rerun after fixing the reported error.
The terminal remains attached to the app; Ctrl-C stops this launch/build.
CARGO_TARGET_DIR is honored (relative paths resolve from your calling directory).
See README.md for setup, account connections and first-launch permissions.
HELP
}

fail() { printf 'run_gui: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || fail "$2"; }

case "${1:-}" in
  -h|--help) usage; exit 0 ;;
  '') ;;
  *) usage >&2; fail "Unknown argument: $1" ;;
esac
[ "$#" -eq 0 ] || fail 'No positional arguments are supported; use --help.'

# Resolve symlinks without GNU readlink -f (the system Bash on macOS is 3.2).
source_path=${BASH_SOURCE[0]}
case "$source_path" in /*) ;; *) source_path="$PWD/$source_path" ;; esac
while [ -L "$source_path" ]; do
  source_dir=$(cd -P -- "${source_path%/*}" && pwd)
  source_path=$(readlink "$source_path")
  case "$source_path" in /*) ;; *) source_path="$source_dir/$source_path" ;; esac
done
root=$(cd -P -- "${source_path%/*}" && pwd)
case "${CARGO_TARGET_DIR:-$root/target}" in
  /*) export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target}" ;;
  *) export CARGO_TARGET_DIR="$PWD/$CARGO_TARGET_DIR" ;;
esac
cd "$root"
[ -f src-tauri/tauri.conf.json ] && [ -f ui/package-lock.json ] ||
  fail 'This script must live in a complete Starkbot Neo source checkout.'

platform=$(uname -s)
case "$platform" in
  Darwin) ;;
  Linux) [ "$(uname -m)" = x86_64 ] || fail 'The Linux desktop currently supports x86-64 hosts.' ;;
  *) fail "Unsupported platform: $platform. Use macOS or Linux." ;;
esac
need rustup 'Install Rust using the instructions at https://rustup.rs, then reopen your terminal.'
need cargo 'Cargo is not on PATH. Load ~/.cargo/env or reopen your terminal after installing Rust.'
need rustc 'rustc is not on PATH. Load ~/.cargo/env or reopen your terminal after installing Rust.'
need node 'Install Node.js 22.12 or newer from https://nodejs.org (with npm), then reopen your terminal.'
need npm 'npm is missing. Install Node.js with npm from https://nodejs.org.'
node -e 'const [major, minor] = process.versions.node.split(".").map(Number); process.exit(major > 22 || (major === 22 && minor >= 12) ? 0 : 1)' ||
  fail 'Node.js 22.12 or newer is required. Upgrade Node.js and reopen your terminal.'

if [ "$platform" = Darwin ]; then
  need xcode-select 'Install Xcode Command Line Tools: xcode-select --install'
  xcode-select -p >/dev/null 2>&1 || fail 'Install Xcode Command Line Tools: xcode-select --install'
  xcrun --find clang >/dev/null 2>&1 || fail 'Xcode cannot locate clang. Finish Command Line Tools installation and accept any Xcode license prompt.'
  need codesign 'The macOS codesign tool is missing. Install Xcode Command Line Tools.'
else
  [ -n "${WAYLAND_DISPLAY:-}${DISPLAY:-}" ] || fail 'No graphical session found. Run this command in a terminal inside your Wayland or X11 desktop (not a headless SSH session).'
  for tool in cc c++ make pkg-config; do
    need "$tool" "Missing $tool. Install your distro's C/C++ build tools and pkg-config; see README.md → Linux prerequisites."
  done
  missing=()
  for library in gtk+-3.0 webkit2gtk-4.1 libsoup-3.0 alsa; do
    pkg-config --exists "$library" || missing+=("$library")
  done
  [ "${#missing[@]}" -eq 0 ] || fail "Missing native development libraries: ${missing[*]}. Install the Linux packages listed in README.md, then rerun ./run_gui.sh."
  # Match neo gui: only NVIDIA + Wayland, and never override a user value.
  if [ "${WEBKIT_DISABLE_DMABUF_RENDERER+x}" != x ] &&
      [ "${WAYLAND_DISPLAY+x}" = x ] && [ -d /sys/module/nvidia_drm ]; then
    export WEBKIT_DISABLE_DMABUF_RENDERER=1
    printf 'run_gui: enabling the NVIDIA/Wayland WebKit DMABUF workaround.\n'
  fi
fi

# Build children get their own process group. Forward interruptions to the
# entire group (npm, Cargo and their children), not just the immediate wrapper.
child=
interrupt() {
  trap '' INT TERM HUP
  if [ -n "$child" ]; then
    kill -s "$1" -- "-$child" 2>/dev/null || true
    wait "$child" 2>/dev/null || true
  fi
  exit "$2"
}
trap 'interrupt INT 130' INT
trap 'interrupt TERM 143' TERM
trap 'interrupt HUP 129' HUP
set -m
run() {
  "$@" &
  child=$!
  local status=0
  wait "$child" || status=$?
  child=
  [ "$status" -eq 0 ] || fail "Command failed (exit $status): $*. Fix the error above, then rerun ./run_gui.sh."
}

printf 'run_gui: preparing the Rust toolchain pinned by rust-toolchain.toml.\n'
run rustup show active-toolchain
rust_info=$(rustc -vV)
host=
while IFS= read -r line; do
  case "$line" in 'host: '*) host=${line#host: } ;; esac
done <<< "$rust_info"
[ -n "$host" ] || fail 'Could not determine the native Rust host target from rustc -vV.'

# Saved manifests live inside node_modules, so deleting dependencies also
# invalidates the cache. Never use source mtimes to decide whether to build.
if [ ! -x ui/node_modules/.bin/vite ] || [ ! -x ui/node_modules/.bin/tsc ] ||
    ! cmp -s ui/package.json ui/node_modules/.neo-package.json ||
    ! cmp -s ui/package-lock.json ui/node_modules/.neo-package-lock.json; then
  printf 'run_gui: installing locked frontend dependencies.\n'
  run npm --prefix "$root/ui" ci --include=dev --include=optional --no-audit --no-fund
  cp ui/package.json ui/node_modules/.neo-package.json
  cp ui/package-lock.json ui/node_modules/.neo-package-lock.json
fi

cli_version=2.11.5
cli_dir="$root/target/run-gui-cli"
tauri="$cli_dir/node_modules/.bin/tauri"
if [ ! -x "$tauri" ] || [ "$("$tauri" --version 2>/dev/null || true)" != "tauri-cli $cli_version" ]; then
  printf 'run_gui: installing checkout-local Tauri CLI %s.\n' "$cli_version"
  mkdir -p "$cli_dir"
  run npm --prefix "$cli_dir" install --no-save --package-lock=false --include=optional --no-audit --no-fund "@tauri-apps/cli@$cli_version"
fi

# Explicit paths avoid discovery scans (including the CLI's own package.json).
export TAURI_APP_PATH="$root/src-tauri"
export TAURI_FRONTEND_PATH="$root/ui"
printf 'run_gui: building the standalone desktop (frontend + incremental Cargo release build).\n'
if [ "$platform" = Darwin ]; then
  # --bundles app overrides bundle.active=false. Tauri merges the platform
  # config automatically, including Info.plist and microphone entitlements.
  # Local builds need no paid certificate, but signing must still apply the
  # audio-input entitlement. An explicitly configured identity takes priority.
  export APPLE_SIGNING_IDENTITY="${APPLE_SIGNING_IDENTITY:--}"
  run "$tauri" build --target "$host" --bundles app -- --locked
  app="$CARGO_TARGET_DIR/$host/release/bundle/macos/Starkbot Neo.app"
  executable="$app/Contents/MacOS/neo-desktop"
else
  run "$tauri" build --target "$host" --no-bundle -- --locked
  executable="$CARGO_TARGET_DIR/$host/release/neo-desktop"
fi
[ -x "$executable" ] || fail "Tauri finished without the expected executable: $executable"

# Execute the bundle's binary on macOS so its Info.plist/signature stay with
# it, while retaining terminal diagnostics and normal signal/exit handling.
printf 'run_gui: launching %s\n' "$executable"
trap - INT TERM HUP
set +m
exec "$executable"
