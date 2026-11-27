# Remote changes

A sync pass needs the remote state of its folder. Listing it costs one request per folder, so a pass lists only when it has to (`src/services/sync/remote_view.rs`).

## The rule

- `sync_items` records the last synced state of every item. After a pass whose remote side matched it item for item ("level"), it also holds the remote state.
- Such a pass stores the backend's change cursor in `sync_dirs.remote_cursor`, together with the time of its last full listing and which paths were left out (exclusions, nested sync folders, hidden folders).
- The next pass asks the backend what changed since the cursor (`BackendClient::changes`):

| Answer | The pass |
|---|---|
| Nothing changed | takes the remote side from `sync_items` and lists nothing |
| These items or folders changed | fetches only those and puts them on top of `sync_items` |
| Can't tell | lists the folder in full |

- A pass that doesn't end level keeps the older cursor, so the next one fetches the same changes again. Changes describe items as they are now, so applying them twice is harmless.
- Other left-out paths, rows without a recorded kind and, for rclone remotes, a full listing older than an hour make the pass list in full.
- Rows record whether the remote item is a folder (`is_dir`), its provider ID (`remote_id`: Proton's link ID, Google Drive's file ID, OneDrive's item ID) and, for folders, a change tag (`remote_tag`, Nextcloud's ETag). The tag is written only by a level pass: a tag newer than the rows would hide changes below the folder that the rows don't have yet.

## Change sources

| Backend | Cursor | Changes | After a restart |
|---|---|---|---|
| Proton | last event of the volume's event log and the sync folder's link ID, asked through Syncative's Proton backend for rclone (`src/go/proton/rclonefs`) | the changed links with their decrypted names, placed by ID | kept |
| Nextcloud | the sync folder's ETag, which the server renews with every change below it | a walk into the subfolders whose ETag changed (`src/go/rclone/davtags.go`) | kept |
| OneDrive | the delta token of the drive's change log and the sync folder's item ID, asked through Syncative's rclone fork (`backend/onedrive/syncative.go`, `src/go/rclone/onedrive.go`) | the changed items with their names, placed by ID: the delta listing gives no paths | kept |
| Google Drive, Dropbox | the provider's change-log token, asked once per pass through Syncative's rclone fork | the folders holding the paths it reported (`src/go/rclone/changes.go`) | one full listing: rclone names Drive's changes through the folders its backend has listed in this run |
| Other rclone backends | none | | every pass lists |

One answer per remote:

- The sync folders of a remote ask one after the other in a pass and share one answer: an answer up to 3 s old stands in for the request (`reuseFor` in `src/go/rclone/changes.go`). The window is shorter than the shortest interval (5 s), so every pass asks at least once.
- Proton, OneDrive, Google Drive, Dropbox: the change log covers the whole remote, so a quiet pass makes one request for all folders. A folder whose cursor stayed behind gets the kept answers since its cursor.
- Nextcloud: the server renews the root's ETag with every change below it, also for a change by the owner of a folder shared with the user (tested on Nextcloud 34). A pass asks the root's ETag once; a folder whose ETag was read under the root's current ETag skips its own request (`tagGate` in `src/go/rclone/davtags.go`). A quiet pass makes one request, a pass with changes one more than without the gate.
- Every ETag request goes through one HTTP client, so its connection stays open between passes.

Google Drive and Dropbox:

- Every sync folder of a remote asks with its own token. The answers stay in memory by the token they start from (the last 64), so all of them hear the same changes: Drive's backend names a renamed or moved folder's old path only once, since the fork then drops that folder from rclone's folder cache, where its old path would otherwise still lead to it.
- The change query uses the backend that rclone's cache holds, the one the listings fill. Two callers can create a remote's backend at the same time; then the later one stays in the cache, and the first pass that sees the switch lists once.

OneDrive:

- An item names its parent by ID, which the sync matches against the IDs the listing recorded (`driveID#itemID`, as rclone's onedrive backend names items). A token too old for the drive (410 Gone, `resyncRequired`) starts the log anew and the pass lists in full.
- A folder shared with the account and added to its OneDrive lives on the owner's drive, whose changes this drive's log doesn't report; the hourly full listing catches them.

More than 64 folders to fetch, a folder without an ETag, the sync folder itself renamed or gone, or a Proton event that can't be read make the pass list in full.

## Paths and IDs

Proton addresses items by link ID, and rclone by path. Syncative's Proton backend finds a folder's ID through rclone's folder cache, as the drive backend does, and a file's in its folder's listing. The last four listings answer for 30 s, so copying a folder's files lists it once, not once per file; a change by another client clears them.

## Times

Proton lists a file with the time its active revision was uploaded, which never drifts, and keeps the time the file was last modified before the upload in the revision's encrypted extended attributes. The sync compares the listed time (rclone's `ModTime`); downloads and the conflict dialog take the other one, which the Proton backend gives as the metadata `mtime`. Proton downloads copy with rclone's `Metadata` option, so the file gets that time from the copy, and `details` asks for it. When Proton doesn't answer for the attributes, the copy and `details` fail and the next pass tries again: the listed time would give old files today's date and reorder galleries sorted by date (issue 25). A network drive shows that time as well (`ModifiedTime` of the Proton backend, `src/go/rclone/modtime.go`): the Linux mount through its wrapper's files, Android's documents provider for files the VFS holds as the remote has them; a file written on the drive shows the VFS's own time until it is listed again.

Up to 0.28, downloads from Proton and files whose time alone changed there could get the listed time. Such a file still has it to the second, in the file and in both times of its row; at the end of the first pass that syncs a Proton folder, before the pass reports the folder synced, `services::sync::dates` gives each one the time from `details` and records the check in `sync_dirs.dates_checked`, so it runs once per folder. A pass that can't ask for a time leaves the flag for the next one.

Testing: *Remote change detection* in [testing.md](testing.md).
