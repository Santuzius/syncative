# Testing

**Never test with a real instance.** Its accounts, keyring entries, database (`~/.local/share/syncative`), config (`~/.config/syncative`) and `$XDG_RUNTIME_DIR/syncative/rclone.conf` sync real data; a test sharing any of them can overwrite tokens or start syncs and deletions. Test only with throwaway accounts or mock remotes, in an isolated instance.

Keep test loops fast: poll every ≈0.5 s and stop as soon as the condition holds, with tight timeouts instead of fixed `sleep 20`. Start a pass with *Sync now* instead of waiting for the interval.

## Desktop
`scripts/test-instance.sh` starts an isolated instance: its own HOME and XDG folders under `/tmp/syncative-test`, its own D-Bus session with its own gnome-keyring, and no Wayland socket. It refuses to start if something else owns the keyring service.

- `scripts/test-instance.sh xvfb` runs headless on display `:99`; `scripts/test-instance.sh desktop` shows the window on the desktop, e.g. to sign in throwaway accounts. Add `--release` for the release build.
- `scripts/test-screenshot.sh` takes screenshots into `temp/` and sends clicks, text and keys to the headless window.
- `scripts/test-seed-mock.sh` adds offline mock remotes: a real two-way sync between two local folders, an unreachable WebDAV remote (error state) and a Proton remote without a session ("Not signed in").
- **Storage line:** LocalMock (an `alias` onto a local dir) answers rclone's `about` with the statfs numbers of that directory's disk, so the storage line in the remote page's header shows offline; OfflineMock (dead port) and ProtonMock (no session) report nothing and show no line.
- `scripts/test-mem.sh` prints the memory use of the test instance.
- Variables: `SYNCATIVE_TEST_RESET=1` wipes the test state, `SYNCATIVE_TEST_BIN` runs another build, `SYNCATIVE_TEST_SHOW=0` starts in the tray, `SYNCATIVE_COLOR_SCHEME=light|dark` forces the theme.
- **The test instance's remotes from the command line:** its `rclone.conf` is encrypted with a password in its keyring. Read it through the instance's D-Bus session (`DBUS_SESSION_BUS_ADDRESS` from `/proc/<pid>/environ` of the running test Syncative): `secret-tool lookup service "Syncative Keys" username rclone-config-password`, then `RCLONE_CONFIG_PASS=… rclone --config /tmp/syncative-test/run/syncative/rclone.conf md5sum GoogleDrive:<folder>`.
- **A file saved while it uploads:** a sync folder with a 5 MB file that a loop replaces every 2 s (write a temporary file, then `mv` over it, as editors do) for about 45 s; afterwards the remote must hold the last version, and the log (with *Detailed log* on, the file `detailed-log` in the data folder) must show no `corrupted on transfer`. Larger files only work on a fast connection.
- **Network loss:** start the instance with `HTTPS_PROXY`/`HTTP_PROXY` pointing at a local CONNECT proxy and kill the proxy, or point them at a dead port such as `127.0.0.1:9`.

## AppImage in containers
On a host with NixOS's `programs.appimage.binfmt`, the kernel hands every AppImage to `/run/binfmt/appimage_type_2`, which doesn't exist inside containers, so AppImages there fail with "No such file or directory", even with `--appimage-extract-and-run`. Unpack them with `unsquashfs -o <offset>` (the offset is the end of the ELF section header table) and run `squashfs-root/AppRun`. `appimage/Dockerfile` already blanks appimagetool's magic bytes for the build.

## Android
`scripts/android-dev.sh` builds the debug APK (with optimised native libraries), installs it on the USB phone and starts it. The debug variant lets `adb shell run-as io.github.santuzius.syncative` reach the app's files.

- **Build time:** about 1–2 minutes, so run the script in the foreground and wait for it to exit; it ends after the app was started or prints the first cargo errors. Don't put it in the background and poll it with `pgrep -f`: the pattern matches the polling shell itself, and the Gradle daemon keeps running for hours after a build. If Nix has to rebuild the Android SDK, e.g. on a new machine or after a nixpkgs update, the first build takes several minutes longer.
- **Offline remote:** run `rclone serve webdav /tmp/syncative-phone-webdav --addr 127.0.0.1:18080 --user test --pass test` on the PC and `adb reverse tcp:18080 tcp:18080`. Upload test files through WebDAV (`curl -T`), not straight into the folder, since `rclone serve` caches listings for 5 minutes.
- **Seeding the remote:** *Add remote* hides WebDAV, so add it through `run-as`: the rows in `files/syncative/data.sqlite`, the section in `files/run/syncative/rclone.conf` and a local folder such as `files/test-sync`. Syncative encrypts `rclone.conf` with rclone's config encryption and rewrites a plain-text file encrypted once at startup, so write the section while the app is stopped and before its config was encrypted, e.g. right after a fresh install.
- **Engine without the window:** reinstalling the APK while Syncative runs sends `MY_PACKAGE_REPLACED` to `BootReceiver`, which starts `SyncService`. A force-stopped app gets no broadcasts until it is launched again.
- **Logs:** the phone's logcat buffer holds only a few minutes. For long background tests stream it to the PC (`adb logcat -v time > file &`) or read the database (`adb exec-out run-as … cat files/syncative/data.sqlite`, timestamps in `sync_items`). Never `pkill -f` a pattern that also matches your own command line.
- **GrapheneOS** behaves differently from stock Android in places: Storage Scopes and its own memory allocator (hardened_malloc).
- Uninstalling the app also removes the permissions granted to it (notifications, All files access).
- If others use the phone too, check the foreground app before sending taps.
