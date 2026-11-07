#!/usr/bin/env bash
# Regenerates assets/licenses.json, the "Open source licenses" in About: the license texts of every Rust crate (cargo-about) and Go package (go-licenses) built into Syncative, with each package's copyright lines (ScanCode). Run it before a release (doc/packaging/release.md) and commit the result.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# ScanCode in its own environment under temp/, which only this script needs. nixpkgs' package doesn't build, and the pinned pluggy is what ScanCode's plugins still work with. Its binary wheels need steam-run on NixOS.
RUN=""
command -v steam-run >/dev/null && RUN="steam-run"
VENV="$REPO/temp/scancode-venv"
if [ ! -x "$VENV/bin/scancode" ]; then
  $RUN python3 -m venv "$VENV"
  $RUN "$VENV/bin/pip" --quiet --disable-pip-version-check install "scancode-toolkit-mini==32.5.0" "pluggy<1.6"
fi

cd "$REPO"
nix-shell -p cargo-about --run "cargo about generate --format json -c scripts/about.toml -o '$TMP/rust.json'"
cd "$REPO/src/go"
nix-shell -p go go-licenses gcc --run "
  CGO_ENABLED=1 go-licenses report . > '$TMP/go.csv' 2>/dev/null
  CGO_ENABLED=1 go-licenses save . --save_path '$TMP/go-save' 2>/dev/null || true
  CGO_ENABLED=1 go list -deps -f '{{with .Module}}{{.Path}} {{.Version}}{{end}}' . | sort -u > '$TMP/go-modules.txt'
  CGO_ENABLED=1 go list -deps -f '{{.ImportPath}} {{.Dir}}' . > '$TMP/go-dirs.txt'
"
python3 "$REPO/scripts/licenses.py" "$TMP" "$RUN $VENV/bin/scancode" > "$TMP/licenses.json"
mv "$TMP/licenses.json" "$REPO/assets/licenses.json"
echo "assets/licenses.json: $(wc -c < "$REPO/assets/licenses.json") bytes"
