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
- Rows record whether the remote item is a folder (`is_dir`), its provider ID (`remote_id`: Proton's link ID, Google Drive's file ID) and, for folders, a change tag (`remote_tag`, Nextcloud's ETag). The tag is written only by a level pass: a tag newer than the rows would hide changes below the folder that the rows don't have yet.

## Change sources

| Backend | Cursor | Changes | After a restart |
|---|---|---|---|
| Proton | last event of the volume's event log and the sync folder's link ID, asked through Syncative's Proton backend for rclone (`src/go/proton/rclonefs`) | the changed links with their decrypted names, placed by ID | kept |
| Nextcloud, ownCloud | the sync folder's ETag, which the server renews with every change below it | a walk into the subfolders whose ETag changed (`src/go/rclone/davtags.go`) | kept |
| Google Drive, Dropbox | the provider's change-log token, asked once per pass through Syncative's rclone fork | the folders holding the paths it reported (`src/go/rclone/changes.go`) | one full listing: rclone names Drive's changes through the folders its backend has listed in this run |
| Other rclone backends | none | | every pass lists |

Google Drive and Dropbox:

- Every sync folder of a remote asks with its own token. The answers stay in memory by the token they start from (the last 64), so all of them hear the same changes: Drive's backend names a renamed or moved folder's old path only once, since the fork then drops that folder from rclone's folder cache, where its old path would otherwise still lead to it.
- The change query uses the backend that rclone's cache holds, the one the listings fill. Two callers can create a remote's backend at the same time; then the later one stays in the cache, and the first pass that sees the switch lists once.

More than 64 folders to fetch, a folder without an ETag, the sync folder itself renamed or gone, or a Proton event that can't be read make the pass list in full.

## Paths and IDs

Proton addresses items by link ID, and rclone by path. Syncative's Proton backend finds a folder's ID through rclone's folder cache, as the drive backend does, and a file's in its folder's listing. The last four listings answer for 30 s, so copying a folder's files lists it once, not once per file; a change by another client clears them.

Testing: *Remote change detection* in [testing.md](testing.md).
