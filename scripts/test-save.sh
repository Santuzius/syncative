#!/usr/bin/env bash
# Hand files to the running test instance (scripts/test-instance.sh) as the file manager's "Save in the cloud with Syncative" does, never to the real one.
#
# Usage: scripts/test-save.sh FILE...
#   SYNCATIVE_TEST_DIR picks the test instance (default /tmp/syncative-test), SYNCATIVE_TEST_BIN the build (default target/debug/syncative).
# For Dolphin, point a service menu's Exec at this script: Exec=/path/to/repo/scripts/test-save.sh %F
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
T="${SYNCATIVE_TEST_DIR:-/tmp/syncative-test}"
BIN="${SYNCATIVE_TEST_BIN:-$REPO/target/debug/syncative}"
LOCK="$T/home/.local/share/syncative/instance.lock"
# Without a running test instance this launch would become one itself, outside the test instance's own keyring.
if [ ! -e "$LOCK" ] || flock -n "$LOCK" true; then
  echo "no test instance runs in $T; start scripts/test-instance.sh first" >&2
  exit 1
fi
export HOME="$T/home" XDG_DATA_HOME="$T/home/.local/share" XDG_CONFIG_HOME="$T/home/.config" XDG_CACHE_HOME="$T/home/.cache" XDG_RUNTIME_DIR="$T/run"
exec "$BIN" --save "$@"
