package netdrive

import (
	"context"
	"os"
	"time"

	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/vfs"
)

// modifiedTimer is a file whose listed time isn't the time it was last modified: Proton lists the time of the upload and keeps the other in the revision (src/go/proton/rclonefs). The sync compares the listed time; a network drive shows the other one, as a gallery sorted by date expects (issue 25).
type modifiedTimer interface {
	ModifiedTime(ctx context.Context) time.Time
}

// shownModTime is the time a network drive shows for file `o`.
func shownModTime(ctx context.Context, o fs.Object) time.Time {
	if m, ok := o.(modifiedTimer); ok {
		return m.ModifiedTime(ctx)
	}
	return o.ModTime(ctx)
}

// nodeModTime is the time a network drive shows for `info`, a node of its VFS: a file as the remote has it shows the time it was last modified, a file written here the time the VFS gives it.
func nodeModTime(info os.FileInfo) time.Time {
	t := info.ModTime()
	file, ok := info.(*vfs.File)
	if !ok {
		return t
	}
	o, ok := file.DirEntry().(fs.Object)
	if !ok || o == nil {
		return t
	}
	ctx := context.Background()
	if !t.Equal(o.ModTime(ctx)) {
		return t
	}
	return shownModTime(ctx, o)
}
