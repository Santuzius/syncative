#!/usr/bin/env bash
# Regenerates assets/licenses.json, the "Open source licenses" in About: the license texts of every Rust crate (cargo-about) and Go package (go-licenses) built into Syncative, with each package's copyright lines. Run it before a release (doc/packaging/release.md) and commit the result.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

cd "$REPO"
nix-shell -p cargo-about --run "cargo about generate --format json -c scripts/about.toml -o '$TMP/rust.json'"
cd "$REPO/src/go"
nix-shell -p go go-licenses gcc --run "
  CGO_ENABLED=1 go-licenses report . > '$TMP/go.csv' 2>/dev/null
  CGO_ENABLED=1 go-licenses save . --save_path '$TMP/go-save' 2>/dev/null || true
  CGO_ENABLED=1 go list -deps -f '{{with .Module}}{{.Path}} {{.Version}}{{end}}' . | sort -u > '$TMP/go-modules.txt'
"
python3 "$REPO/scripts/licenses.py" "$TMP" > "$REPO/assets/licenses.json"
echo "assets/licenses.json: $(wc -c < "$REPO/assets/licenses.json") bytes"
