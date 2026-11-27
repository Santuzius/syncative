// Package nextcloud reaches what rclone's WebDAV backend doesn't on a Nextcloud server: the folder ETags that tell what changed (tags.go) and the trash bin (trash.go). The requests use the remote's own options and rclone's HTTP client.
package nextcloud
