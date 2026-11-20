//go:build linux && !android

package main

// rclone's FUSE mount for the network drive mode: it registers rclone's remote-control calls mount/mount and mount/unmount, which Rust sends through RcloneRPC. Users need fusermount3 (fuse3). Android has no FUSE for apps and gets a document provider instead.
import _ "github.com/rclone/rclone/cmd/mount"
