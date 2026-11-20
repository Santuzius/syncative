# Network drive

The fourth folder mode, *Network drive* 🧪 (`SyncMode::NetworkDrive`, a beta feature), syncs nothing. The folder's local path shows the remote folder through rclone's VFS, which downloads a file when it is opened and keeps recently used ones in its cache; changes are uploaded when a file is closed.

## Linux

- `src/go/mount_linux.go` links rclone's FUSE mount (`cmd/mount`), which registers rclone's remote-control calls `mount/mount` and `mount/unmount`. Rust sends them through the ordinary RPC call (`src/services/network_drive.rs`); no export of its own is needed.
- The mount uses `--vfs-cache-mode full`. The cache lies in rclone's default place, `~/.cache/rclone/vfs/<remote>/`.
- Users need `fusermount3` (the `fuse3` package). Without it the folder shows rclone's error and the hint to install it.
- **When it mounts:** a pass for the folder mounts it if this process hasn't yet, so it follows the remote's pause and the engine's schedule like any folder. Proton remotes work through Syncative's own rclone backend, Google Drive through rclone's.
- **When it unmounts:** when the folder or its remote is removed (the engine's reload, `network_drive::retain`) and when the app quits from the tray. A Syncative that dies with a mount leaves a dead mount point ("Transport endpoint is not connected"); the next mount unmounts it lazily with `fusermount3 -u -z` first.
- **The folder must be empty** (or missing), as the mount hides what is in it. The add dialog refuses a folder with content.
- **No switching:** a synced folder and a network drive hold different things locally (a full copy, or nothing), so the folder settings don't switch between them; the folder is removed and added again.

## Android

Not available yet; the mode isn't offered there. The plan is a document provider (`DocumentsProvider` in Java, registered in `AndroidManifest.xml`) that calls a few Go functions on the same VFS: list, open, read, write, close.

## Changes by other clients

rclone's VFS keeps a folder's listing for five minutes (`--dir-cache-time`). Each pass of a network drive folder asks the mounted backend's change log what changed since the previous pass and has the VFS forget those paths (`syncative/mount-poll`, an RPC call of Syncative's in `src/go/rclone/networkdrive.go`), so the mount follows the remote's interval and pause like a synced folder; rclone's own poll timer is off (`PollInterval` 0). Google Drive and Dropbox answer from the fork's change-log calls, Proton (`src/go/proton/rclonefs`) from the volume's event log. That log names a moved file by its new parent only, so the Proton backend remembers where it listed each file once a mount follows the log, and the VFS forgets the folder the file left as well. A WebDAV mount has no change log and shows other clients' changes after the five minutes.

## Open points

- Whether GNOME Files and Dolphin list the mount in their sidebars, and whether Files by Google on Android shows other apps' document providers, is still to be checked.

Testing: add a network drive folder in the test instance (with `beta-features` in its data dir), then read and write through the mount and watch the change arrive in a synced folder of the same remote folder.
