#!/usr/bin/env bash
# Makes a release as doc/packaging/release.md describes: bumps the version, regenerates the licenses, builds and checks the APK and the AppImage, pins the AppImage for Nix, commits and tags, pushes develop, main and the tag, and creates a draft GitHub release with the files. Publishing the draft stays manual; .github/workflows/nix-release.yml then checks the Nix package. Run it from develop, by hand or through .github/workflows/release.yml.
#   scripts/release.sh X.Y.Z                    full release; notes from temp/release/notes-X.Y.Z.md if present, else a placeholder to edit on GitHub
#   scripts/release.sh X.Y.Z --dry-run          builds and checks the APK and the AppImage, then puts the release files back; no commit, tag, push or release
# SYNCATIVE_KEYSTORE_PROPERTIES names a keystore.properties to use when android/keystore.properties is absent (the runner's copy).
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"
APK_CERT_SHA256="6dd390bb5a6ffc6abb6a71620688550927d9f5b883ca076414388e3aa52f8743"
RELEASE_FILES=(Cargo.toml Cargo.lock assets/licenses.json nix/release.json)

die() { echo "release: $*" >&2; exit 1; }
step() { echo; echo "== $*"; }

version="${1:-}"
dry_run=0
[ "${2:-}" = "--dry-run" ] && dry_run=1
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "usage: $0 X.Y.Z [--dry-run]"
tag="v$version"
apk="Syncative-$version-arm64-v8a.apk"
appimage="Syncative-$version-x86_64.AppImage"
out="$REPO/temp/release"

step "Checks before building"
[ "$(git rev-parse --abbrev-ref HEAD)" = "develop" ] || die "not on develop"
[ ! -f .cargo/config.toml ] || die ".cargo/config.toml is present: Cargo.lock would point at local checkouts of the forks"
git fetch --quiet --tags origin develop main
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/develop)" ] || die "develop differs from origin/develop; push or pull first"
git merge-base --is-ancestor origin/main HEAD || die "origin/main is not an ancestor of develop"
! git rev-parse -q --verify "refs/tags/$tag" >/dev/null || die "tag $tag exists already"
# Only the files this script writes may be changed, so a run that failed halfway can start again.
dirty="$(git status --porcelain --untracked-files=no | awk '{print $2}' | grep -vxF -f <(printf '%s\n' "${RELEASE_FILES[@]}") || true)"
[ -z "$dirty" ] || die "uncommitted changes outside the release files: $dirty"
if [ ! -f android/keystore.properties ]; then
  [ -n "${SYNCATIVE_KEYSTORE_PROPERTIES:-}" ] || die "android/keystore.properties is missing and SYNCATIVE_KEYSTORE_PROPERTIES is not set"
  cp "$SYNCATIVE_KEYSTORE_PROPERTIES" android/keystore.properties
  trap 'rm -f "$REPO/android/keystore.properties"' EXIT
fi
command -v docker >/dev/null || die "docker is missing"
command -v gh >/dev/null || die "gh is missing"

step "Bump to $version"
sed -i "0,/^version = \".*\"$/s//version = \"$version\"/" Cargo.toml
grep -qx "version = \"$version\"" Cargo.toml || die "could not set the version in Cargo.toml"
nix-shell shell.nix --run 'cargo metadata --format-version 1 >/dev/null'
scripts/update-licenses.sh

step "Build the APK and the AppImage"
rm -f "$out/$apk" "$out/$appimage" "$out/$appimage.zsync" "$out/SHA256SUMS"
scripts/android-release.sh
scripts/build-appimage.sh
[ -f "$out/$apk" ] && [ -f "$out/$appimage" ] && [ -f "$out/$appimage.zsync" ] || die "a build output is missing in temp/release"

step "Check the APK"
code="$(echo "$version" | awk -F. '{print ($1 * 1000 + $2) * 1000 + $3}')"
APK="$out/$apk" CERT="$APK_CERT_SHA256" CODE="$code" nix-shell android/shell.nix --run '
  bt="$(ls -d "$ANDROID_HOME"/build-tools/*/ | sort -V | tail -1)"
  "${bt}apksigner" verify --print-certs "$APK" | grep -q "SHA-256 digest: $CERT" || { echo "release: the APK is not signed with the release key" >&2; exit 1; }
  badging="$("${bt}aapt2" dump badging "$APK" | head -1)"
  [[ "$badging" == *"versionCode='"'"'$CODE'"'"'"* ]] || { echo "release: the APK'"'"'s versionCode is not $CODE: $badging" >&2; exit 1; }
' 2> >(grep -v 'making symlink' >&2)
echo "APK: release key, versionCode $code"

step "Check the AppImage in other distributions"
# Piped in instead of mounted, with bytes 8-10 blanked: on a NixOS host with programs.appimage.binfmt the kernel would otherwise hand the file to appimage-run, which the containers lack.
for image in ubuntu:24.04 debian:12 fedora:42; do
  case "$image" in
    fedora*) install="dnf install -qy dbus-libs >/dev/null" ;;
    *) install="apt-get update -qq && apt-get install -qqy libdbus-1-3 >/dev/null" ;;
  esac
  docker run --rm -i "$image" sh -c "cat > a && $install && printf '\0\0\0' | dd of=a bs=1 seek=8 conv=notrunc 2>/dev/null && chmod +x a && ./a --appimage-extract-and-run --help >/dev/null" < "$out/$appimage" || die "the AppImage doesn't start in $image"
  echo "AppImage: starts in $image"
done

if [ "$dry_run" = 1 ]; then
  git checkout -- "${RELEASE_FILES[@]}"
  echo
  echo "Dry run done: the APK and the AppImage for $version build and pass the checks (in temp/release). Nothing was committed or pushed."
  exit 0
fi

step "Pin the AppImage for Nix"
hash="$(nix hash file "$out/$appimage")"
printf '{\n  "version": "%s",\n  "hash": "%s"\n}\n' "$version" "$hash" > nix/release.json

step "Commit and tag"
git add -- "${RELEASE_FILES[@]}"
git commit --quiet -m ":bookmark: Bump crate to $version" -- "${RELEASE_FILES[@]}"
git tag -a "$tag" -m "Syncative $version"
git log --oneline -1

step "Push develop, main and $tag"
git push origin HEAD:develop HEAD:main "$tag"
# Moves the local main along if it exists and isn't checked out.
git show-ref -q --verify refs/heads/main && git branch -f main HEAD

step "Draft release"
cd "$out"
sha256sum "$apk" "$appimage" > SHA256SUMS
if [ -f "notes-$version.md" ]; then notes=(--notes-file "notes-$version.md"); else notes=(--notes "Release notes still to be written."); fi
gh release create "$tag" --draft --verify-tag --latest --title "Syncative $version" "${notes[@]}" "$apk" "$appimage" "$appimage.zsync" SHA256SUMS
echo
echo "Done. Check the notes and publish the draft on GitHub; the workflow Nix release then checks the Nix package."
