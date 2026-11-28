package netdrive

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"testing"
	"time"

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
	if got, ok := drives[dir].(rc.Params); !ok || !reflect.DeepEqual(got["files"], []driveFile{{Name: "a.txt"}}) || got["moving"] != true || len(drives) != 1 {
		t.Fatalf("downloading: %v", drives)
	}
	tr.Done(ctx, nil)
	drives = transfers()
	if got, ok := drives[dir].(rc.Params); !ok || len(got["files"].([]driveFile)) != 0 || len(got["finished"].([]finishedFile)) != 1 {
		t.Fatalf("done: %v", drives)
	}
}

// TestFinishedTransfersKeepTheirError: a failed transfer of the drive comes with its error, a cancelled upload and other drives' transfers are left out.
func TestFinishedTransfersKeepTheirError(t *testing.T) {
	at := time.Unix(1700000000, 0)
	transferred := []accounting.TransferSnapshot{
		{Name: "a.txt", SrcFs: "cache", DstFs: "G:x", CompletedAt: at, Error: errors.New("quota exceeded")},
		{Name: "b.txt", SrcFs: "G:x", CompletedAt: at},
		{Name: "c.txt", SrcFs: "cache", DstFs: "G:x", CompletedAt: at, Error: fmt.Errorf("upload: %w", context.Canceled)},
		{Name: "d.txt", SrcFs: "G:y", CompletedAt: at},
	}
	want := []finishedFile{
		{Name: "a.txt", Upload: true, Error: "quota exceeded", At: at.UnixNano()},
		{Name: "b.txt", At: at.UnixNano()},
	}
	if got := finishedFiles(transferred, "G:x"); !reflect.DeepEqual(got, want) {
		t.Fatalf("got %v", got)
	}
}
