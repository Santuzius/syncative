package changes

import (
	"context"
	"fmt"
	"io"
	"os"
	"strings"
	"testing"
	"time"

	"syncative/go/rclone"

	"github.com/rclone/rclone/fs/cache"
	"github.com/rclone/rclone/fs/config/obscure"
	"github.com/rclone/rclone/fs/operations"
)

// TestChangesSinceFollowsNextcloudTags: on a Nextcloud server, given as for TestTrashRestoresFromNextcloud, the sync's change query takes the folder's ETag for its cursor and names the folder once something below it changed.
func TestChangesSinceFollowsNextcloudTags(t *testing.T) {
	url, user, pass := os.Getenv("SYNCATIVE_NEXTCLOUD_URL"), os.Getenv("SYNCATIVE_NEXTCLOUD_USER"), os.Getenv("SYNCATIVE_NEXTCLOUD_PASS")
	if url == "" || user == "" || pass == "" {
		t.Skip("no Nextcloud server given")
	}
	t.Setenv("RCLONE_CONFIG_CHANGETESTNC_TYPE", "webdav")
	t.Setenv("RCLONE_CONFIG_CHANGETESTNC_URL", url)
	t.Setenv("RCLONE_CONFIG_CHANGETESTNC_VENDOR", "nextcloud")
	t.Setenv("RCLONE_CONFIG_CHANGETESTNC_USER", user)
	t.Setenv("RCLONE_CONFIG_CHANGETESTNC_PASS", obscure.MustObscure(pass))
	defer func(d time.Duration) { rclone.AnswerReuse = d }(rclone.AnswerReuse)
	rclone.AnswerReuse = 0
	ctx := context.Background()
	f, err := cache.Get(ctx, "changetestnc:")
	if err != nil {
		t.Fatal(err)
	}
	dir := fmt.Sprintf("SyncativeChangeTest-%d", time.Now().UnixNano())
	t.Cleanup(func() { _ = operations.Purge(context.Background(), f, dir) })
	if err := f.Mkdir(ctx, dir); err != nil {
		t.Fatal(err)
	}

	first, err := ChangesSince(ctx, "changetestnc", dir, "")
	if err != nil || !first.Supported || first.Known || first.Cursor == "" {
		t.Fatalf("first: %+v, %v", first, err)
	}
	quiet, err := ChangesSince(ctx, "changetestnc", dir, first.Cursor)
	if err != nil || !quiet.Known || quiet.Cursor != first.Cursor || len(quiet.Folders) != 0 {
		t.Fatalf("quiet: %+v, %v", quiet, err)
	}
	if _, err := operations.Rcat(ctx, f, dir+"/a.txt", io.NopCloser(strings.NewReader("a")), time.Now(), nil); err != nil {
		t.Fatal(err)
	}
	changed, err := ChangesSince(ctx, "changetestnc", dir, first.Cursor)
	if err != nil || !changed.Known || changed.Cursor == first.Cursor || len(changed.Folders) != 1 || changed.Folders[0] != dir {
		t.Fatalf("changed: %+v, %v", changed, err)
	}
	missing, err := ChangesSince(ctx, "changetestnc", dir+"/missing", "")
	if err != nil || !missing.Supported || missing.Cursor != "" {
		t.Fatalf("missing: %+v, %v", missing, err)
	}
}
