# MEGA: research notes

MEGA was requested in [issue 2](https://github.com/Santuzius/syncative/issues/2). These notes collect what was found in October 2026 before deciding whether to build it.

## Why not rclone's Mega backend
Checked against rclone master (`backend/mega/mega.go`, July 2026), which builds on [go-mega](https://github.com/t3rm1n4l/go-mega) (MIT):
- **No content check:** `Hashes()` returns `hash.None`. go-mega's `FileAttr` decodes only the name (`n`), so MEGA's fingerprint attribute `c` is never read.
- **No modification times:** `Precision()` returns `ModTimeNotSupported`, `SetModTime()` fails; `ModTime()` is the upload time.
- **No version history:** `Update()` uploads a new file and then deletes the old one (to the rubbish bin unless `hard_delete`), so the old content is not kept as a version.
- **Attributes get lost:** go-mega's `Rename` and `Upload.Finish` write attributes containing only the name, which wipes `c` and every other attribute.
- Newer rclone versions added a `2fa` option and session reuse.

Without fingerprints, Syncative's `same_content` (`src/services/sync/applier.rs`) can't tell whether two files are equal, and the planner falls back to "newer wins" for files that are new on both sides.

## Planned approach (not started)
- **Go:** an own package `src/go/mega/` on top of go-mega, next to `src/go/drive/` for Proton:
  - sign-in with 2FA and session reuse
  - upload with the fingerprint `c`
  - versions through `ov`
  - read-modify-write of all attributes on rename and move
  - mtime through setattr
  - deletes into the rubbish bin
  - change detection through MEGA's event stream
- **Rust:** a MEGA `BackendClient` (`src/domain/ports.rs`), the fingerprint computed locally and used by `same_content`, and an *Add remote* form with e-mail, password and 2FA code.
- **Checks** against MEGA's official web and Android apps: files uploaded there must show the same fingerprint and mtime as Syncative's, and the other way round.
- **Effort:** about two weeks, plus one or two days for a disk cache, then a beta test with the requester.

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
- Fingerprint compatibility, including the old overflow variant and files over 4 GB.
- mtime precision and every attribute key MEGA's apps write.
- `ov` versions, and whether an account can switch versioning off.
- Event stream reliability, rate limits and the transfer quota.
- Sign-in: v2 accounts, 2FA, resuming sessions.
- Duplicate names in one folder and MEGA's name rules.
- Shared folders.
- Licences and terms of service.
- Partial fetch for normal sessions, and the order of nodes in the `f` response.

## Alternatives for users
MEGA's own clients already sync in both directions: the desktop app (Linux, Windows, macOS) syncs the whole drive or selected folders, and the Android app syncs or backs up device folders. Syncing incoming shared folders works only on the desktop; the iOS app has no folder sync. [MEGA Help](https://help.mega.io/mobile-apps/mobile-backup-sync/set-up-android-sync), [mega.io/desktop](https://mega.io/desktop).
