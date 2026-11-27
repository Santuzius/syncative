// Package netdrive is the network drive folder mode: a mount follows the remote's changes at the sync interval (poll.go), keeps a trash of its own (trashfs.go, trashbin.go), shows the time a file was last modified (modtime.go) and tells what it transfers (drivetransfers.go); on Android a document provider stands in for the mount (documents.go).
package netdrive
