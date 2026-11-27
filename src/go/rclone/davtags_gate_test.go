package rclone

import (
	"context"
	"fmt"
	"io"
	"net/http/httptrace"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/rclone/rclone/fs/cache"
	"github.com/rclone/rclone/fs/config/obscure"
	"github.com/rclone/rclone/fs/operations"
)

// server is a WebDAV tree's ETags as the gate sees them.
type server struct {
	tags  map[string]string
	asked []string
}

func (s *server) ask(dir string) (string, error) {
	s.asked = append(s.asked, dir)
	return s.tags[dir], nil
}

// change gives `dir` and every folder above it a new ETag, as Nextcloud does.
func (s *server) change(dirs ...string) {
	for _, d := range dirs {
		s.tags[d] += "'"
	}
}

func TestQuietFoldersShareTheRootsETag(t *testing.T) {
	srv := &server{tags: map[string]string{"": "r", "Notes": "n", "Taxes": "t"}}
	g := &tagGate{seen: map[string]seenTag{}}
	pass := func() (notes, taxes string) {
		notes, _ = g.folderTag("Notes", srv.ask)
		taxes, _ = g.folderTag("Taxes", srv.ask)
		return
	}
	defer func(d time.Duration) { reuseFor = d }(reuseFor)

	pass()
	if len(srv.asked) != 3 {
		t.Fatalf("first pass asked %v", srv.asked)
	}
	// A later quiet pass asks only the root.
	reuseFor = 0
	srv.asked = nil
	if n, tx := pass(); n != "n" || tx != "t" || len(srv.asked) != 2 || srv.asked[0] != "" || srv.asked[1] != "" {
		t.Fatalf("quiet pass asked %v: %s %s", srv.asked, n, tx)
	}
	// Within one pass the root is asked once.
	reuseFor = time.Minute
	g.rootAt = time.Time{}
	srv.asked = nil
	pass()
	if len(srv.asked) != 1 {
		t.Fatalf("one pass asked %v", srv.asked)
	}
	// A change below Taxes: the root and Taxes change, both are asked, Notes keeps its ETag.
	reuseFor = 0
	srv.change("Taxes", "")
	srv.asked = nil
	if n, tx := pass(); n != "n" || tx != "t'" {
		t.Fatalf("after a change: %s %s, asked %v", n, tx, srv.asked)
	}
	if want := []string{"", "Notes", "", "Taxes"}; len(srv.asked) != len(want) {
		t.Fatalf("after a change asked %v, want %v", srv.asked, want)
	}
	// The root itself as a sync folder is always asked.
	srv.asked = nil
	if r, _ := g.folderTag("", srv.ask); r != "r'" || len(srv.asked) != 1 {
		t.Fatalf("root: %s, asked %v", r, srv.asked)
	}
}

// TestFolderTagsOnNextcloud reads folder ETags through the gate on a real server, given as for TestTrashRestoresFromNextcloud: a change shows in its folder only, and all requests share one connection.
func TestFolderTagsOnNextcloud(t *testing.T) {
	url, user, pass := os.Getenv("SYNCATIVE_NEXTCLOUD_URL"), os.Getenv("SYNCATIVE_NEXTCLOUD_USER"), os.Getenv("SYNCATIVE_NEXTCLOUD_PASS")
	if url == "" || user == "" || pass == "" {
		t.Skip("no Nextcloud server given")
	}
	t.Setenv("RCLONE_CONFIG_TAGTESTNC_TYPE", "webdav")
	t.Setenv("RCLONE_CONFIG_TAGTESTNC_URL", url)
	t.Setenv("RCLONE_CONFIG_TAGTESTNC_VENDOR", "nextcloud")
	t.Setenv("RCLONE_CONFIG_TAGTESTNC_USER", user)
	t.Setenv("RCLONE_CONFIG_TAGTESTNC_PASS", obscure.MustObscure(pass))
	defer func(d time.Duration) { reuseFor = d }(reuseFor)
	reuseFor = 0
	dials := 0
	ctx := httptrace.WithClientTrace(context.Background(), &httptrace.ClientTrace{GotConn: func(c httptrace.GotConnInfo) {
		if !c.Reused {
			dials++
		}
	}})
	f, err := cache.Get(ctx, "tagtestnc:")
	if err != nil {
		t.Fatal(err)
	}
	base := fmt.Sprintf("SyncativeTagTest-%d", time.Now().UnixNano())
	t.Cleanup(func() { _ = operations.Purge(context.Background(), f, base) })
	for _, d := range []string{"Notes", "Taxes"} {
		if err := f.Mkdir(ctx, base+"/"+d); err != nil {
			t.Fatal(err)
		}
	}
	tags := func() (notes, taxes string) {
		notes, _, err := FolderTags(ctx, "tagtestnc", base+"/Notes", 0)
		if err != nil {
			t.Fatal(err)
		}
		taxes, _, err = FolderTags(ctx, "tagtestnc", base+"/Taxes", 0)
		if err != nil {
			t.Fatal(err)
		}
		return
	}
	notes, taxes := tags()
	dials = 0
	if n, tx := tags(); n != notes || tx != taxes {
		t.Fatalf("quiet: %s %s, was %s %s", n, tx, notes, taxes)
	}
	if _, err := operations.Rcat(ctx, f, base+"/Taxes/b.txt", io.NopCloser(strings.NewReader("b")), time.Now(), nil); err != nil {
		t.Fatal(err)
	}
	if n, tx := tags(); n != notes || tx == taxes {
		t.Fatalf("after a change in Taxes: %s %s, was %s %s", n, tx, notes, taxes)
	}
	if dials > 1 {
		t.Fatalf("%d new connections for the ETag requests", dials)
	}
}

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
	defer func(d time.Duration) { reuseFor = d }(reuseFor)
	reuseFor = 0
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
