#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
REFERENCE="$ROOT/spikes/fixtures/golden/chrome"
ACTUAL="$ROOT/spikes/out/golden/webkit"
DIFF="$ROOT/spikes/out/golden/diff"
REPORT="$ROOT/spikes/out/golden/report.json"
MODE="${1:---compare}"

export_chrome() {
  mkdir -p "$REFERENCE"
  cargo run --quiet -p s7-render --bin export_fixture_goldens -- "$REFERENCE"
}

capture_webkit() {
  rm -rf "$ACTUAL" "$DIFF"
  mkdir -p "$ACTUAL" "$DIFF"
  NEO_WEBKIT_CAPTURE_DIR="$ACTUAL" \
  NEO_WEBKIT_OUT="$ROOT/spikes/out/golden/webkit-metrics.json" \
    cargo run --quiet -p s7-webview
}

compare() {
  if [[ ! -f "$REFERENCE/flex-launch.png" ]]; then
    echo "Chrome references are missing; run $0 --update first" >&2
    exit 2
  fi
  cargo run --quiet -p s7-render --bin golden_regression -- \
    "$REFERENCE" "$ACTUAL" "$DIFF" | tee "$REPORT"
}

cd "$ROOT"
case "$MODE" in
  --update)
    export_chrome
    ;;
  --capture)
    capture_webkit
    ;;
  --compare)
    capture_webkit
    compare
    ;;
  --all)
    export_chrome
    capture_webkit
    compare
    ;;
  *)
    echo "usage: $0 [--update|--capture|--compare|--all]" >&2
    exit 2
    ;;
esac
