package netdrive

import (
	"fmt"
	"os"
	"strings"
	"testing"
	"time"

	"syncative/go/rclone/nextcloud"

	"github.com/rclone/rclone/fs/config/obscure"
	"github.com/rclone/rclone/fs/operations"
)

// TestTrashRestoresFromNextcloud trashes a cached file on a Nextcloud server and restores it from Nextcloud's trash bin. It runs only against a server given by SYNCATIVE_NEXTCLOUD_URL (the WebDAV URL, e.g. http://localhost:8082/remote.php/dav/files/USER), SYNCATIVE_NEXTCLOUD_USER and SYNCATIVE_NEXTCLOUD_PASS; use a throwaway account.
func TestTrashRestoresFromNextcloud(t *testing.T) {
	url, user, pass := os.Getenv("SYNCATIVE_NEXTCLOUD_URL"), os.Getenv("SYNCATIVE_NEXTCLOUD_USER"), os.Getenv("SYNCATIVE_NEXTCLOUD_PASS")
	if url == "" || user == "" || pass == "" {
		t.Skip("no Nextcloud server given")
	}
	t.Setenv("RCLONE_CONFIG_TRASHTESTNC_TYPE", "webdav")
	t.Setenv("RCLONE_CONFIG_TRASHTESTNC_URL", url)
	t.Setenv("RCLONE_CONFIG_TRASHTESTNC_VENDOR", "nextcloud")
	t.Setenv("RCLONE_CONFIG_TRASHTESTNC_USER", user)
	t.Setenv("RCLONE_CONFIG_TRASHTESTNC_PASS", obscure.MustObscure(pass))
	root := fmt.Sprintf("SyncativeTrashTest-%d", time.Now().UnixNano())
	tt := newTrashTestOn(t, "trashtestnc", root)
	t.Cleanup(func() { _ = operations.Purge(tt.ctx, tt.f.inner, "") })

	if err := tt.vfs.WriteFile("note.txt", []byte("from the server"), 0o644); err != nil {
		t.Fatal(err)
	}
	tt.vfs.WaitForWriters(10 * time.Second)
	tt.trash("note.txt")
	if _, err := tt.f.inner.NewObject(tt.ctx, "note.txt"); err == nil {
		t.Fatal("the file is still on the server")
	}
	if _, ok := tt.f.readEntry("note.txt"); !ok {
		t.Fatal("no trash entry")
	}

	if err := tt.vfs.Rename(tt.prefix+"/files/note.txt", "note.txt"); err != nil {
		t.Fatal(err)
	}
	o, err := tt.f.inner.NewObject(tt.ctx, "note.txt")
	if err != nil {
		t.Fatal(err)
	}
	if got := o.Size(); got != int64(len("from the server")) {
		t.Fatalf("restored size = %d", got)
	}
	// Restored from the trash bin, not uploaded: the trash bin no longer holds it.
	if err := nextcloud.Restore(tt.ctx, "trashtestnc", root+"/note.txt", time.Now()); err == nil || !strings.Contains(err.Error(), "holds no") {
		t.Fatalf("the trash bin still held the file: %v", err)
	}
}
