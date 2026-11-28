# MEGA: research notes and progress

MEGA was requested in [issue 2](https://github.com/Santuzius/syncative/issues/2). These notes collect what was found in October 2026 and the progress since November 2026, when work on a beta provider started.

## What rclone's Mega backend lacks upstream
Checked against rclone master (`backend/mega/mega.go`, July 2026), which builds on [go-mega](https://github.com/t3rm1n4l/go-mega) (MIT):
- **No content check:** `Hashes()` returns `hash.None`. go-mega's `FileAttr` decodes only the name (`n`), so MEGA's fingerprint attribute `c` is never read.
- **No modification times:** `Precision()` returns `ModTimeNotSupported`, `SetModTime()` fails; `ModTime()` is the upload time.
- **No version history:** `Update()` uploads a new file and then deletes the old one (to the rubbish bin unless `hard_delete`), so the old content is not kept as a version.
- **Attributes get lost:** go-mega's `Rename` and `Upload.Finish` write attributes containing only the name, which wipes `c` and every other attribute.
- Newer rclone versions added a `2fa` option and session reuse.

Without fingerprints, Syncative's `same_content` (`src/engine/folder/applier/conflicts.rs`) can't tell whether two files are equal, and the planner falls back to "newer wins" for files that are new on both sides.

## Plan
MEGA goes through rclone's Mega backend, patched in Syncative's [rclone fork](https://github.com/Santuzius/rclone) (branch `syncative`), which uses a fork of go-mega ([Santuzius/go-mega](https://github.com/Santuzius/go-mega), branch `syncative`, by a `replace` in the rclone fork's `go.mod`). The network drive needs rclone anyway, and the MEGA SDK would add a large C++ build (Crypto++, curl, ICU, libsodium, SQLite) for desktop, Android, Nix, AppImage and Snap.
- **go-mega fork:**
  - package `fingerprint`: the fingerprint, a `Sampler` that collects it from an upload's chunks in any order, and the stored form `c`
  - `FileAttr` keeps every attribute it doesn't know, so rename and move no longer wipe `c`
  - uploads write `c` with the file's mtime; `SetModTime` rewrites it through setattr
  - `Upload.SetReplaces` sends `ov`, so the upload becomes a new version of the old file
- **rclone fork, `backend/mega`:** mtimes with one-second precision (`ModTime`, `SetModTime`), `Update` as a new version instead of upload and delete, and the metadata keys `mtime` and `mega-crc` (the 16 CRC bytes in hex).
- **Syncative:** `ProviderKind::Mega` as a beta provider with e-mail, password and 2FA code (`src/services/auth/mega.rs`); `HashKind::Mega`, computed locally in `src/engine/folder/mega_crc.rs`.
- **Checks** against MEGA's official web and Android apps: files uploaded there must show the same fingerprint and mtime as Syncative's, and the other way round.

The fingerprint reaches the sync as metadata, not as an rclone hash: rclone's hash interface (`RegisterHash` in `fs/hash/hash.go`) streams a file from its start without knowing its size, while the sampled CRC needs the size to place its windows. A registered hash would also be offered by the local backend and compared by rclone itself, which would report files with the legacy CRC as corrupted. `operations/fsinfo` lists `mega-crc` among the metadata, so `hash_names` (`src/infrastructure/rclone/rpc.rs`) adds the pseudo-name `mega`, and `file_details` reads the CRC from the metadata; `content_hash::matching` accepts either variant as the same content.

## Progress
- **Done:** everything under *Plan*. The fingerprint passes the values of MEGA's SDK tests in Go and Rust, including the legacy variant. Syncative's `src/go/go.mod` points to the rclone fork by pseudo-version.
- **Remote checklist** ([testing.md](../development/testing.md#checklist-for-a-remote)) passed on the desktop (root sync) and on Android (one folder), with a throwaway account and the web app as the other client:
  - Fingerprints Syncative writes (5 B to 150 MB, including sizes past the legacy overflow) equal those the web app writes for the same files, and the web app shows the uploaded modification times.
  - A changed file becomes a new version of the old one (`ov`), as in the web app.
  - Time-only changes settle in one pass on both sides; a remote one counts as `download=1` in the plan line but only takes over the time.
  - Renames and moves arrive as a download plus a local delete, as with every provider.
  - Trash and restore, the sync folder's own remote folder renamed or trashed, changes while the app was stopped, and a network loss during an upload behave as the checklist expects.
  - Removing the remote deletes its database rows and its `rclone.conf` section; MEGA keeps nothing else on disk.
- **Noticed:** while offline, go-mega's event poll logs `pollEvents: Error fetching status` every 5 s.
- **Next:**
  1. Change detection: whether rclone's polling is enough or MEGA's event stream is needed.
  2. Memory for large accounts (see below), possibly a disk cache.
  3. Before a release: tag the rclone fork, use the tag in `src/go/go.mod`, update `vendorHash` in `nix/package.nix`.
- **Related open pull requests on go-mega:** [#65](https://github.com/t3rm1n4l/go-mega/pull/65) and [#66](https://github.com/t3rm1n4l/go-mega/pull/66) add the fingerprint and mtime; #65 encodes it in standard base64, where MEGA uses the URL alphabet without padding, and neither keeps attributes other than the name and fingerprint.

The official desktop client [MEGAsync](https://github.com/meganz/MEGAsync) is under the "MEGA Limited Code Licence", which allows review but not reuse, so its code can't be used. The MEGA SDK ([meganz/sdk](https://github.com/meganz/sdk)) is BSD-2 and serves as the reference for the formats below.

## The fingerprint
From the SDK's `src/filefingerprint.cpp`:
- It covers the size, the mtime and four 32-bit CRC values (16 bytes).
- Files up to 16 bytes: the raw bytes, padded with zeros.
- Files up to 8192 bytes: one CRC32 over each quarter of the file.
- Larger files are sampled: per CRC lane 32 blocks of 64 bytes at offset `(size − 64) · i / 127`, clamped to the file.
- The CRC values are stored in network byte order (`htonl`).
- Stored form (`serializefingerprint`): base64 of the 16 CRC bytes followed by the mtime as one length byte plus its bytes in little-endian order.
- The mtime is compared with a tolerance (`FS_MTIME_TOLERANCE_SECS`).
- Older SDKs computed the sample offsets with a 32-bit overflow. Since January 2026 the SDK accepts both variants; Syncative must do the same.

## Memory and caching
- go-mega keeps the whole account tree in memory (`MegaFS.lookup`, plus keys per node). Rough estimate: 10 000 nodes ≈ 5 MB (15–20 MB peak while loading), 100 000 ≈ 50 MB (150 MB peak), 1 000 000 ≈ 500 MB (1.5 GB peak).
- The SDK's fetch command `f` (`c=1, r=1`, `ca=1` for its cache) fetches the whole tree. A partial fetch (`n=<root>, part=1`) is sent only for the password-manager client type; whether it works for normal sessions is untested.
- **Disk cache idea:** keep the node tree in SQLite with the keys still encrypted as received, the master key in the keyring, and the event-stream position, so the full fetch happens only once. Names outside synced folders stay encrypted on disk.

## Open questions
- Fingerprint compatibility with files from old clients (legacy variant) and files over 4 GB.
- mtime precision and every attribute key MEGA's apps write.
- Whether an account can switch versioning off, and what `ov` does then.
- Event stream reliability, rate limits and the transfer quota.
- Sign-in: v2 accounts, 2FA, resuming sessions.
- Duplicate names in one folder and MEGA's name rules.
- Shared folders.
- Licences and terms of service.
- Partial fetch for normal sessions, and the order of nodes in the `f` response.

## Alternatives for users
MEGA's own clients already sync in both directions: the desktop app (Linux, Windows, macOS) syncs the whole drive or selected folders, and the Android app syncs or backs up device folders. Syncing incoming shared folders works only on the desktop; the iOS app has no folder sync. [MEGA Help](https://help.mega.io/mobile-apps/mobile-backup-sync/set-up-android-sync), [mega.io/desktop](https://mega.io/desktop).
