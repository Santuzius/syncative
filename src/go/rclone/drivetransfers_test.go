package rclone

import (
	"context"
	"os"
	"path/filepath"
	"testing"

	"github.com/rclone/rclone/fs/accounting"
	"github.com/rclone/rclone/fs/cache"
	"github.com/rclone/rclone/fs/rc"
	"github.com/rclone/rclone/vfs"
	"github.com/rclone/rclone/vfs/vfscommon"
)

// TestDriveTransfersNameTheDrive: a file the VFS reads from its backend counts as a download of that drive, and only of that one.
func TestDriveTransfersNameTheDrive(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "a.txt"), []byte("hello"), 0o600); err != nil {
		t.Fatal(err)
	}
	f, err := cache.Get(ctx, dir)
	if err != nil {
		t.Fatal(err)
	}
	opt := vfscommon.Opt
	v := vfs.New(ctx, f, &opt)
	defer v.Shutdown()
	transfers := func() rc.Params {
		out, err := rcDriveTransfers(ctx, rc.Params{"fs": []string{dir, t.TempDir()}})
		if err != nil {
			t.Fatal(err)
		}
		return out["drives"].(rc.Params)
	}

	if drives := transfers(); len(drives) != 0 {
		t.Fatalf("idle: %v", drives)
	}
	o, err := f.NewObject(ctx, "a.txt")
	if err != nil {
		t.Fatal(err)
	}
	tr := accounting.GlobalStats().NewTransfer(o, nil)
	drives := transfers()
	if got, ok := drives[dir].(rc.Params); !ok || got["downloading"] != 1 || got["uploading"] != 0 || got["moving"] != true || len(drives) != 1 {
		t.Fatalf("downloading: %v", drives)
	}
	tr.Done(ctx, nil)
	if drives := transfers(); len(drives) != 0 {
		t.Fatalf("done: %v", drives)
	}
}
