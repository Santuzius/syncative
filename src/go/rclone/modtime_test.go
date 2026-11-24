package rclone

import (
	"context"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/vfs"
	"github.com/rclone/rclone/vfs/vfscommon"

	_ "github.com/rclone/rclone/backend/local"
)

// uploadTimeFs lists every file with the time `listed`, while the file was last modified at `modified`, as Proton does.
type uploadTimeFs struct {
	fs.Fs
	listed, modified time.Time
}

type uploadTimeObject struct {
	fs.Object
	f *uploadTimeFs
}

func (o *uploadTimeObject) ModTime(context.Context) time.Time { return o.f.listed }

func (o *uploadTimeObject) ModifiedTime(context.Context) time.Time { return o.f.modified }

func (f *uploadTimeFs) List(ctx context.Context, dir string) (fs.DirEntries, error) {
	entries, err := f.Fs.List(ctx, dir)
	for i, entry := range entries {
		if o, ok := entry.(fs.Object); ok {
			entries[i] = &uploadTimeObject{Object: o, f: f}
		}
	}
	return entries, err
}

func (f *uploadTimeFs) NewObject(ctx context.Context, remote string) (fs.Object, error) {
	o, err := f.Fs.NewObject(ctx, remote)
	if err != nil {
		return nil, err
	}
	return &uploadTimeObject{Object: o, f: f}, nil
}

func TestDriveShowsTheTimeLastModified(t *testing.T) {
	ctx := context.Background()
	root := t.TempDir()
	if err := os.WriteFile(filepath.Join(root, "photo.jpg"), []byte("jpg"), 0o644); err != nil {
		t.Fatal(err)
	}
	local, err := fs.NewFs(ctx, root)
	if err != nil {
		t.Fatal(err)
	}
	f := &uploadTimeFs{Fs: local, listed: time.Unix(1_795_511_898, 0), modified: time.Unix(1_554_437_228, 0)}

	o, err := f.NewObject(ctx, "photo.jpg")
	if err != nil {
		t.Fatal(err)
	}
	if got := (&wrappedObject{Object: o}).ModTime(ctx); !got.Equal(f.modified) {
		t.Errorf("Linux mount shows %v, want %v", got, f.modified)
	}

	opt := vfscommon.Opt
	opt.CacheMode = vfscommon.CacheModeOff
	v := vfs.New(ctx, f, &opt)
	t.Cleanup(v.Shutdown)
	node, err := v.Stat("photo.jpg")
	if err != nil {
		t.Fatal(err)
	}
	if got := nodeModTime(node); !got.Equal(f.modified) {
		t.Errorf("Android drive shows %v, want %v", got, f.modified)
	}
}
