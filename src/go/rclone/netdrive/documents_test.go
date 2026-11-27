package netdrive

import (
	"context"
	"os"
	"path/filepath"
	"testing"

	"github.com/rclone/rclone/fs/cache"
	"github.com/rclone/rclone/fs/config"
	"github.com/rclone/rclone/fs/rc"
)

func TestDetachUploadsWhatOpenFilesWrote(t *testing.T) {
	dir := t.TempDir()
	oldCache := config.GetCacheDir()
	if err := config.SetCacheDir(filepath.Join(dir, "cache")); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = config.SetCacheDir(oldCache); cache.Clear() })
	remote := filepath.Join(dir, "remote")
	if err := os.MkdirAll(remote, 0o755); err != nil {
		t.Fatal(err)
	}
	ctx := context.Background()
	in := rc.Params{"fs": remote}

	if _, err := rcDocAttach(ctx, in); err != nil {
		t.Fatal(err)
	}
	opened, err := rcDocOpen(ctx, rc.Params{"fs": remote, "path": "a.txt", "flags": int64(linuxWrOnly | linuxCreat)})
	if err != nil {
		t.Fatal(err)
	}
	id := opened["handle"].(int64)
	if _, err := DocWrite(id, []byte("kept"), 0); err != nil {
		t.Fatal(err)
	}

	if _, err := rcDocDetach(ctx, in); err != nil {
		t.Fatal(err)
	}

	got, err := os.ReadFile(filepath.Join(remote, "a.txt"))
	if err != nil || string(got) != "kept" {
		t.Fatalf("a.txt on the remote: %q, %v; want the written text", got, err)
	}
	if _, err := DocWrite(id, []byte("x"), 0); err != ErrBadHandle {
		t.Fatalf("the handle stays open after detach: %v", err)
	}
}
