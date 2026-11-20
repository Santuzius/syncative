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
- Rows record whether the remote item is a folder (`is_dir`), its provider ID (`remote_id`, Proton's link ID) and, for folders, a change tag (`remote_tag`, Nextcloud's ETag). The tag is written only by a level pass: a tag newer than the rows would hide changes below the folder that the rows don't have yet.

## Change sources

| Backend | Cursor | Changes | After a restart |
|---|---|---|---|
| Proton | last event of the volume's event log and the sync folder's link ID | the changed links with their decrypted names, placed by ID | kept |
| Nextcloud, ownCloud | the sync folder's ETag, which the server renews with every change below it | a walk into the subfolders whose ETag changed (`src/go/rclone/davtags.go`) | kept |
| Google Drive, Dropbox | the provider's change-log token, asked once per pass through Syncative's rclone fork | the folders holding the paths it reported (`src/go/rclone/changes.go`) | one full listing: rclone names Drive's changes through the folders its backend has listed in this run |
| Other rclone backends | none | | every pass lists |

More than 64 folders to fetch, a folder without an ETag, the sync folder itself renamed or gone, or a Proton event that can't be read make the pass list in full.

## Paths and IDs

Proton addresses items by link ID. During a pass the client finds them in the pass's own listing (`BackendClient::use_listing`), which carries every item's ID, and in what the pass created itself; anything else walks from the deepest known folder.

Testing: *Remote change detection* in [testing.md](testing.md).
