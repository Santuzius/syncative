# OneDrive

OneDrive is a beta provider. It goes through rclone's onedrive backend, from Syncative's rclone fork.

## Sign-in

- *Connect* runs rclone's OAuth flow in-process (`src/go/authorize.go`) with rclone's built-in app ID, which all rclone users share; the user only signs in with the Microsoft account in the browser. rclone's docs suggest an own client ID when the shared one gets throttled ([rclone.org/onedrive](https://rclone.org/onedrive/)); Syncative doesn't ask for one.
- The config of an onedrive remote also needs the drive (`drive_id`, `drive_type`). After the token, Syncative asks `GET /me/drive`, the account's own drive, and passes the drive with the token to `config/create`, non-interactive: the backend's config would go on to ask on the terminal which drive to use.
- Only the account's own drive is offered. SharePoint sites and other drives of a work account would need the drive choice of `rclone config`.

## Changes

The drive's change log is its delta listing, which names items by ID and gives no paths (Remarks of [driveItem: delta](https://learn.microsoft.com/en-us/graph/api/driveitem-delta)). The fork's `backend/onedrive/syncative.go` asks it once per pass; the sync places the items by ID like Proton's ([remote changes](../development/remote-changes.md)), the network drive by the folders rclone's folder cache knows ([network drive](../development/network-drive.md)).

## Not tested yet

Nothing of it has run against a real account: sign-in, the drive lookup, the ID format of listings and of the delta listing (`driveID#itemID`), deleted items in the delta listing, the network drive. Before it leaves the beta, run the [remote checklist](../development/testing.md#checklist-for-a-remote) with a throwaway Microsoft account.
