#!/usr/bin/env bash
# Builds temp/release/Syncative-<version>-x86_64.AppImage in an Ubuntu 22.04 container (appimage/Dockerfile), so it runs on distributions with glibc 2.35 or newer. Needs Docker. Cargo, Go and the build directory are cached in the Docker volume syncative-appimage-cache.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
out="$REPO/temp/release"
src="$REPO/temp/appimage-src"
mkdir -p "$out"
docker build -q -t syncative-appimage "$REPO/appimage" >/dev/null
# A copy of the working tree without build output, including the proton/api submodule.
rm -rf "$src"
mkdir -p "$src"
tar -C "$REPO" --exclude=./target --exclude=./temp --exclude=./result --exclude=./android --exclude=./.git --exclude=./.cargo/config.toml -cf - . | tar -C "$src" -xf -
docker volume create syncative-appimage-cache >/dev/null
# The cache is shared by everyone who builds on this machine (the release runner has its own user), so files a different user left are handed over first.
docker run --rm -v syncative-appimage-cache:/cache alpine find /cache \( ! -user "$(id -u)" -o ! -group "$(id -g)" \) -exec chown -h "$(id -u):$(id -g)" {} +
docker run --rm --user "$(id -u):$(id -g)" -e HOME=/cache -v syncative-appimage-cache:/cache -v "$src:/src:ro" -v "$out:/out" syncative-appimage /opt/build-inside.sh
rm -rf "$src"
ls -l "$out"/Syncative-*-x86_64.AppImage*
