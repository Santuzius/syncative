# OneDrive

OneDrive is a beta provider. It goes through rclone's onedrive backend, from Syncative's rclone fork.

## Sign-in

- *Connect* runs rclone's OAuth flow in-process (`src/go/authorize.go`) with rclone's built-in app ID, which all rclone users share; the user only signs in with the Microsoft account in the browser. rclone's docs suggest an own client ID when the shared one gets throttled ([rclone.org/onedrive](https://rclone.org/onedrive/)); Syncative doesn't ask for one.
- The config of an onedrive remote also needs the drive (`drive_id`, `drive_type`). After the token, Syncative asks `GET /me/drive`, the account's own drive, and passes the drive with the token to `config/create`, non-interactive: the backend's config would go on to ask on the terminal which drive to use.
- Only the account's own drive is offered. SharePoint sites and other drives of a work account would need the drive choice of `rclone config`.

## Changes

The drive's change log is its delta listing, which names items by ID and gives no paths (Remarks of [driveItem: delta](https://learn.microsoft.com/en-us/graph/api/driveitem-delta)). The fork's `backend/onedrive/syncative.go` asks it once per pass; the sync places the items by ID like Proton's ([remote changes](../development/remote-changes.md)), the network drive by the folders rclone's folder cache knows ([network drive](../development/network-drive.md)).

The delta listing also names every folder above a changed item. A folder rclone's folder cache knows and that stands where the cache has it is left out; one that is gone or stands elsewhere leaves the cache with everything below it, so a sync folder renamed or trashed on OneDrive shows "Remote folder not found" instead of being found under its old path.

OneDrive keeps no SHA-1 of files but its QuickXorHash. `details` asks each backend for the hash rclone lists for it (`operations/fsinfo`), and the sync hashes the local copy the same way (`src/engine/folder/content_hash.rs`) to tell identical files apart from changed ones; Dropbox has its content hash.

## Personal Vault

A personal drive has a Personal Vault in its root, named in the account's language ("Personal Vault", "Persönlicher Tresor"). No API call lists it: listing it fails with "invalidResourceId: ObjectHandle is Invalid" ([rclone#8736](https://github.com/rclone/rclone/issues/8736)), which made the full listing of a sync folder at the drive's root fail. The fork leaves it out of every listing and reports it as gone in the change log, so neither a sync folder nor the network drive shows it. It recognises the vault as a shortcut to a folder in the account's own site; a shortcut to a folder someone shared points into their site and stays.

## Tested

The [remote checklist](../development/testing.md#checklist-for-a-remote) ran on the desktop test instance with a throwaway personal Microsoft account, rclone standing in for the web app and the Graph API's `restore` for the recycle bin: steps 1 to 7 pass. Network loss relies on the system reporting it, which cancels the pass for every provider; a provider that stops answering while the network stays up fails only after rclone's retries, minutes during an upload, as with the other rclone providers.

Not tested yet: removing the remote (step 8), the network drive, and Android.
