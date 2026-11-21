# Making a release

A release ships three things for x86_64 Linux and arm64 Android:

- the signed APK, which Obtainium picks up
- the AppImage with its `.zsync` file for updates
- for Nix, the same program: the flake downloads it from the AppImage (`nix/package-bin.nix`), so nothing is compiled and no binary cache is needed

The branch flow:

- Work happens on `develop` or a feature branch.
- `main` is GitHub's default branch and always points at the latest release, so its README and `PRIVACY.md` (linked from Google's consent screen) describe what users get. `github:Santuzius/syncative` also reads `nix/release.json` from `main`.
- Tags are `vX.Y.Z`.

## Prerequisites

- **Clean `Cargo.lock`:**
  - Without `.cargo/config.toml`: its `[patch]` sections point Cargo at local checkouts of the forks and put path sources into `Cargo.lock`.
  - The forks (iced, iced_android, android-activity) must be pushed and tagged ([below](#the-android-forks)). Nix and the AppImage build fetch them from GitHub.
- **Docker** for the AppImage, **rustup with the Android targets** and the Nix shells for the APK (see [android/README.md](../../android/README.md)).
- **`android/keystore.properties`** with the release key (below).

## Steps

`scripts/release.sh X.Y.Z` does all of this from `develop`, by hand or [through GitHub Actions](#releasing-through-github-actions). It runs the [checks](#checks-before-announcing-it) on the APK and the AppImage before it commits and tags, and creates the release of step 6 as a draft, with the notes from `temp/release/notes-X.Y.Z.md` if that file exists. Publish the draft on GitHub, then run `scripts/release.sh --check-nix X.Y.Z`. A run that fails before the commit can simply be started again. The steps it follows:

1. **Bump** the version in `[workspace.package]` of `Cargo.toml`, then run `cargo metadata` in the dev shell so `Cargo.lock` follows. Run `scripts/update-licenses.sh` (the first run installs ScanCode into `temp/scancode-venv/`), which rewrites `assets/licenses.json` (the "Open source licenses" in About) from the licenses of all Rust crates and Go packages. Don't commit yet.
2. **Build the APK and the AppImage** from this tree. Both write to `temp/release/`:
   ```sh
   scripts/android-release.sh    # Syncative-X.Y.Z-arm64-v8a.apk
   scripts/build-appimage.sh     # Syncative-X.Y.Z-x86_64.AppImage and .zsync
   ```
3. **Pin the AppImage for Nix:** put the version and the hash of exactly this file into `nix/release.json`:
   ```sh
   nix hash file temp/release/Syncative-X.Y.Z-x86_64.AppImage
   ```
   The flake downloads the AppImage from the release under this hash. Never rebuild the AppImage after this step: a new build has a different hash.
4. **Commit** `Cargo.toml`, `Cargo.lock`, `assets/licenses.json` and `nix/release.json` together (`:bookmark: Bump crate to X.Y.Z`). Tag that commit (`git tag -a vX.Y.Z -m "Syncative X.Y.Z"`) and move `develop` and `main` to it.
5. **Push** the branches and the tag: `git push origin develop main vX.Y.Z`.
6. **Release** with notes in the style of [Zed's](https://github.com/zed-industries/zed/releases): one sentence naming the main changes, then `## Features` grouped by `### Area` (Proton Drive, Android, Nix, Other …) and `## Bug Fixes`, plus `## Updating` if users have to do something. One bullet per change, usually one short sentence, starting with Added, Improved, Changed, Reduced or Fixed; the details belong in the commit, not the notes. Patch releases are just a flat list of such bullets:
   ```sh
   cd temp/release
   sha256sum Syncative-X.Y.Z-arm64-v8a.apk Syncative-X.Y.Z-x86_64.AppImage > SHA256SUMS
   gh release create vX.Y.Z --verify-tag --latest --title "Syncative X.Y.Z" --notes-file notes.md \
     Syncative-X.Y.Z-arm64-v8a.apk Syncative-X.Y.Z-x86_64.AppImage Syncative-X.Y.Z-x86_64.AppImage.zsync SHA256SUMS
   ```

## Checks before announcing it

- **APK:**
  - `apksigner verify --print-certs` shows the release certificate, with SHA-256 `6dd390bb5a6ffc6abb6a71620688550927d9f5b883ca076414388e3aa52f8743`.
  - `aapt2 dump badging` shows the new versionCode.
- **AppImage:** It runs `--help` in `ubuntu:24.04`, `debian:12` and `fedora:42` containers (with `--appimage-extract-and-run` and the distribution's dbus library). It needs nothing but glibc 2.35 and `libdbus-1`.
  - Pipe the file into the container (`docker run -i … sh -c 'cat > a'`) instead of mounting it, and blank its bytes 8–10 there (`printf '\0\0\0' | dd of=a bs=1 seek=8 conv=notrunc`): on a NixOS host with `programs.appimage.binfmt`, the kernel otherwise hands the file to `appimage-run`, which the container lacks, and the start fails with "not found".
- **Nix:** `nix build github:Santuzius/syncative/vX.Y.Z` downloads the AppImage without compiling anything, and `result/bin/syncative --help` runs. This only works once the release is published, because the flake fetches the AppImage from it.

## Releasing through GitHub Actions

`.github/workflows/release.yml` runs `scripts/release.sh` on a self-hosted runner labelled `syncative-release`. Start it under Actions → Release → Run workflow, on `develop`, with the version.

- **The runner** is the NixOS service `services.github-runners.<name>`, with `extraLabels = [ "syncative-release" ]`, a fine-grained personal access token in `tokenFile`, and `systemd.services.github-runner-<name>.wantedBy = lib.mkForce [ ];` so it doesn't start at boot. Start it with `systemctl start` only for a release and stop it afterwards: while it is stopped, no job reaches the machine.
- **Its user** is a dedicated system user in the `docker` group, with rustup and the Android targets in its home, which lies outside `/home` because the service hides `/home` (`ProtectHome`), and with the release key: `SYNCATIVE_KEYSTORE_PROPERTIES` (in the service's `extraEnvironment`) names its `keystore.properties`, which the script copies into the checkout for the build. `extraPackages` needs at least `docker`, `gh`, `git`, `nix`, `python3` and `steam-run`.
- **Forks:** the repository setting Settings → Actions → General → "Approval for running fork pull request workflows from contributors" is "Require approval for all external contributors". A pull request from a fork can add a workflow that targets this runner, and it runs only once its workflows are approved, so approve one only after reading every changed file. The milder options would let anyone with one merged change run code on the runner without approval.
- **Pushing:** the run commits as the person who started it and pushes `develop`, `main` and the tag with the workflow's token.

## The Android forks

Syncative uses iced_android and its two forks by tag, so other apps can use the same tested state. A change to any of them is a new iced_android release:

1. **Pick the version** `X.Y.Z` of iced_android: a bug fix raises Z, anything else Y (0.x has no stable API).
2. **Tag the forks** `iced_android-vX.Y.Z`: the iced fork on `android-0.14`, the android-activity fork on `process-lifetime-0.6`, even if one didn't change. Push the tags.
3. **In iced_android:** set `version` in `Cargo.toml`, the tags in `Cargo.toml`, `examples/runtime/Cargo.toml` and the README to the new ones, run `cargo update` so `Cargo.lock` follows, commit, tag `vX.Y.Z`, and push branch and tag.
4. **In Syncative:** set the same tags in `Cargo.toml` (`iced` twice, `iced_android`, `android-activity`, `iced_widget` in `[patch.crates-io]`), run `cargo update`, set the changed entries of `outputHashes` in `nix/package.nix` to `lib.fakeHash` and copy the hashes from the failing `nix build .#syncative-source`.

Cargo treats the same repository under two different tags as two different sources, so all of them must name the same release.

## The rclone fork

Syncative takes rclone from its fork [Santuzius/rclone](https://github.com/Santuzius/rclone) (`replace` in `src/go/go.mod`). The fork exports the one-off change-log query of the drive and dropbox backends (`ChangesStartToken`, `ChangesSince`), which `src/go/rclone/changes.go` asks once per sync pass; upstream runs it only on ChangeNotify's timer. It also drops the folders a change moved or removed from the drive backend's folder cache, and exports `Untrash`, which takes a file or folder out of Drive's trash by ID for the network drive's trash (`src/go/rclone/trashbin.go`). Its drive backend renews an access token Google refuses with a 401 and retries once, so such a refusal, which happens right after the machine wakes from hibernation, isn't read as a dead sign-in. It is maintained as a fork, without a pull request upstream.

- **Branch `syncative`:** an rclone release tag plus the patch commits on top. Each state Syncative uses is tagged `vX.Y.Z-syncative.N`: the rclone release and a counter for the patches. The branch `syncative-v1.69` keeps the patches on v1.69.0, which releases up to 0.26.x build from.
- **Upgrading rclone:** rebase the patch commits onto the new release tag (`git rebase --onto vNEW vOLD syncative`), tag `vNEW-syncative.1`, push branch and tag, set the tag in the `replace` of `src/go/go.mod` and the `require` of `github.com/rclone/rclone` to `vNEW`, run `go mod tidy`, and update `vendorHash` in `nix/package.nix`. Then run the remote checklist ([testing.md](../development/testing.md#checklist-for-a-remote)) for Google Drive and Proton, which both go through rclone.
- **go-crypto:** rclone requires upstream `ProtonMail/go-crypto`, which Go ranks above Proton's `-proton` build of the same version, while go-proton-api's gopenpgp needs the `-proton` build. A `replace` in `src/go/go.mod` pins it; move it along when rclone raises the version.
- **go-proton-api:** after moving the submodule `src/go/proton/api` to another commit, set the same `rev` (and a new `hash`) for `protonApi` in `nix/package.nix`.
- **`vendorHash`:** set it to `lib.fakeHash` and copy the hash from the failing build, or build the Go modules alone: `nix-build -E 'with import <nixpkgs> {}; (buildGoModule { pname = "syncative-go"; version = "0"; src = ./src; modRoot = "go"; vendorHash = lib.fakeHash; }).goModules'` from the repository root, with `src/go/vendor` absent.

## The APK release key

- Android installs an update only over an APK signed with the same key. If the key is lost, users have to uninstall and lose their sign-ins, and Obtainium stops updating. Keep the keystore backed up and its password in a password manager.
- `android/keystore.properties`, not committed, names the key; `android/app/build.gradle` reads it. Without the file the release APK stays unsigned and is not published by mistake with the debug key.
- The versionCode follows from the version: 0.21.1 → 21001.
- Releases up to 0.20.0 had no APK. Development builds are signed with each computer's debug key and have to be uninstalled once before the release APK installs.

## The Nix package

- **On x86_64 (`default`, `syncative`):** `nix/package-bin.nix` takes the program out of the release's AppImage. `autoPatchelfHook` then points it at glibc, libdbus and libgcc from nixpkgs, which come prebuilt from `cache.nixos.org`.
  - Users need no binary cache of their own.
  - `inputs.nixpkgs.follows` doesn't matter.
- **From source:** `syncative-source` (or `programs.syncative.fromSource = true;`) builds from the checkout with `nix/package.nix`. On aarch64 every package builds from source, since there is no AppImage for it.
- **Why not a binary cache:** Nix uses a cache only if the administrator allows it (`trusted-users` or `/etc/nix/nix.conf`); a flake cannot add one by itself. 0.21.0 and 0.21.1 used Cachix (`celeste.cachix.org`, now unused), which only worked on NixOS through the module and without `follows`.

## The AppImage

- **Updating from Celeste:** AppImages released as Celeste (up to 0.22.2) look for `Celeste-*-x86_64.AppImage.zsync` in the latest release. Releases no longer include it, so these AppImages don't update to Syncative by themselves.
- `appimage/Dockerfile` builds in Ubuntu 22.04, so glibc 2.35 is the minimum.
- `appimage/build-inside.sh` packs the AppDir with appimagetool's static runtime: no libfuse2 needed, only the system's `fusermount`.
- The update information (`gh-releases-zsync|Santuzius|syncative|latest|Syncative-*-x86_64.AppImage.zsync`) lets Gear Lever and AppImageUpdate find newer releases.
- Syncative writes the path of the `.AppImage` file (`$APPIMAGE`) into its autostart entry, because the binary inside lies in a mount point that changes with every start.
- Tested in a Kubuntu 24.04 VM, under X11 and Wayland:
  - Autostart after a reboot works.
  - The tray icon appears.
  - Syncative keeps running after the window closes.
  - A second start brings up the running instance's window.

## AppImageHub
- Syncative's entry in the [AppImageHub](https://appimage.github.io) catalogue came from [AppImage/appimage.github.io#10458](https://github.com/AppImage/appimage.github.io/pull/10458). Its test always uses the latest GitHub release.
- To refresh the entry after a release, comment `/retest` on that merged PR: the bot opens a new PR that tests the release again and merges itself.
- The catalogue shows a screenshot taken by the bot, unless the AppImage ships `usr/share/metainfo/*.metainfo.xml` with a screenshot URL (planned: `doc/img/Screenshot_light.png` on raw.githubusercontent.com).
