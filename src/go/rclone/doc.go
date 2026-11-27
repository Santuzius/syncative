// Package rclone holds Syncative's additions to rclone that the others share: RPC calls with a cancel flag (call.go) and the environment Android hands over (environment.go). The rest is in packages by topic:
//   - changes: what changed on a remote since the last sync pass, for the sync and the network drive alike.
//   - nextcloud: Nextcloud's folder ETags and trash bin, which rclone's WebDAV backend doesn't reach.
//   - netdrive: the network drive: polling a mount, its trash, Android's document provider, its transfers.
package rclone
