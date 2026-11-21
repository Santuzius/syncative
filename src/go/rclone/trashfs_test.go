package rclone

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/fs/cache"
	"github.com/rclone/rclone/fs/config"
	"github.com/rclone/rclone/vfs"
	"github.com/rclone/rclone/vfs/vfscommon"
)

// trashTest is a network drive of a local folder standing in for the remote, mounted through the trash wrapper into a VFS with a full cache, as Syncative mounts it.
type trashTest struct {
	t      *testing.T
	ctx    context.Context
	remote string
	base   string
	f      *trashFs
	vfs    *vfs.VFS
	prefix string
}

func newTrashTest(t *testing.T) *trashTest {
	t.Helper()
	remote := filepath.Join(t.TempDir(), "remote")
	if err := os.MkdirAll(remote, 0o755); err != nil {
		t.Fatal(err)
	}
	t.Setenv("RCLONE_CONFIG_TRASHTESTREMOTE_TYPE", "local")
	return newTrashTestOn(t, "trashtestremote", remote)
}

// newTrashTestOn mounts folder `root` of configured remote `name`.
func newTrashTestOn(t *testing.T, name, root string) *trashTest {
	t.Helper()
	dir := t.TempDir()
	base := filepath.Join(dir, "trash-of-folder")
	oldCache := config.GetCacheDir()
	if err := config.SetCacheDir(filepath.Join(dir, "cache")); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = config.SetCacheDir(oldCache) })
	ctx := context.Background()
	fsString := fmt.Sprintf(`:%s,remote=%s,dir="%s":%s`, TrashBackend, name, base, root)
	f, err := cache.Get(ctx, fsString)
	if err != nil {
		t.Fatal(err)
	}
	opt := vfscommon.Opt
	opt.CacheMode = vfscommon.CacheModeFull
	opt.WriteBack = fs.Duration(10 * time.Millisecond)
	opt.PollInterval = 0
	opt.HandleCaching = 0
	v := vfs.New(ctx, f, &opt)
	t.Cleanup(func() {
		v.WaitForWriters(5 * time.Second)
		v.Shutdown()
		_ = v.CleanUp()
		cache.Clear()
	})
	return &trashTest{t: t, ctx: ctx, remote: root, base: base, f: f.(*trashFs), vfs: v, prefix: fmt.Sprintf(".Trash-%d", os.Getuid())}
}

func (tt *trashTest) write(rel, content string) {
	tt.t.Helper()
	p := filepath.Join(tt.remote, rel)
	if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
		tt.t.Fatal(err)
	}
	if err := os.WriteFile(p, []byte(content), 0o644); err != nil {
		tt.t.Fatal(err)
	}
}

// cache has the VFS fetch `rel` in full, as opening it in an app does.
func (tt *trashTest) cache(rel string) {
	tt.t.Helper()
	if _, err := tt.vfs.ReadFile(rel); err != nil {
		tt.t.Fatal(err)
	}
}

// trash does what KIO does to trash `rel`: write the .trashinfo, then rename the file into files/.
func (tt *trashTest) trash(rel string) {
	tt.t.Helper()
	name := filepath.Base(rel)
	for _, d := range []string{tt.prefix, tt.prefix + "/info", tt.prefix + "/files"} {
		if err := tt.vfs.Mkdir(d, 0o700); err != nil && !os.IsExist(err) {
			tt.t.Fatal(err)
		}
	}
	info := fmt.Sprintf("[Trash Info]\nPath=%s\nDeletionDate=2026-11-21T12:00:00\n", rel)
	if err := tt.vfs.WriteFile(tt.prefix+"/info/"+name+".trashinfo", []byte(info), 0o600); err != nil {
		tt.t.Fatal(err)
	}
	if err := tt.vfs.Rename(rel, tt.prefix+"/files/"+name); err != nil {
		tt.t.Fatal(err)
	}
	tt.vfs.WaitForWriters(5 * time.Second)
}

func exists(p string) bool {
	_, err := os.Lstat(p)
	return err == nil
}

func (tt *trashTest) local(rel string) string {
	return filepath.Join(tt.base, "trash", rel)
}

func TestTrashKeepsCachedCopyAndRestores(t *testing.T) {
	tt := newTrashTest(t)
	tt.write("Recipes/soup.txt", "lentils")
	tt.cache("Recipes/soup.txt")

	tt.trash("Recipes/soup.txt")
	if exists(filepath.Join(tt.remote, "Recipes/soup.txt")) {
		t.Fatal("the file is still on the remote")
	}
	if exists(filepath.Join(tt.remote, tt.prefix)) {
		t.Fatal("the trash folder reached the remote")
	}
	if got, _ := os.ReadFile(tt.local("files/soup.txt")); string(got) != "lentils" {
		t.Fatalf("local copy = %q", got)
	}
	if !exists(tt.local("info/soup.txt.trashinfo")) {
		t.Fatal("no .trashinfo in the local trash")
	}
	if entry, ok := tt.f.readEntry("soup.txt"); !ok || entry.Path != "Recipes/soup.txt" {
		t.Fatalf("entry = %+v, %v", entry, ok)
	}

	// Restore as KIO does: move it back, then delete the .trashinfo. A local remote has no trash, so the copy is uploaded.
	if err := tt.vfs.Rename(tt.prefix+"/files/soup.txt", "Recipes/soup.txt"); err != nil {
		t.Fatal(err)
	}
	if err := tt.vfs.Remove(tt.prefix + "/info/soup.txt.trashinfo"); err != nil {
		t.Fatal(err)
	}
	if got, _ := os.ReadFile(filepath.Join(tt.remote, "Recipes/soup.txt")); string(got) != "lentils" {
		t.Fatalf("restored = %q", got)
	}
	if exists(tt.local("files/soup.txt")) || exists(tt.f.entryPath("soup.txt")) {
		t.Fatal("the trash entry is left after the restore")
	}
}

func TestTrashUncachedLeavesNothing(t *testing.T) {
	tt := newTrashTest(t)
	tt.write("big.iso", "not fetched")

	tt.trash("big.iso")
	if exists(filepath.Join(tt.remote, "big.iso")) {
		t.Fatal("the file is still on the remote")
	}
	if exists(tt.local("files/big.iso")) || exists(tt.local("info/big.iso.trashinfo")) {
		t.Fatal("an uncached file left a trash entry")
	}
	// The VFS lists the trash anew after the forget.
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		entries, _ := tt.vfs.ReadDir(tt.prefix + "/info")
		if len(entries) == 0 {
			return
		}
		time.Sleep(100 * time.Millisecond)
	}
	t.Fatal("the VFS still lists the dropped .trashinfo")
}

func TestTrashFolderKeepsCachedFiles(t *testing.T) {
	tt := newTrashTest(t)
	tt.write("Photos/a.jpg", "a")
	tt.write("Photos/sub/b.jpg", "b")
	tt.cache("Photos/sub/b.jpg")

	tt.trash("Photos")
	if exists(filepath.Join(tt.remote, "Photos")) {
		t.Fatal("the folder is still on the remote")
	}
	if exists(tt.local("files/Photos/a.jpg")) {
		t.Fatal("an uncached file was kept")
	}
	if got, _ := os.ReadFile(tt.local("files/Photos/sub/b.jpg")); string(got) != "b" {
		t.Fatalf("kept = %q", got)
	}
	if entry, ok := tt.f.readEntry("Photos"); !ok || !entry.Dir {
		t.Fatalf("entry = %+v, %v", entry, ok)
	}
}

func TestTrashHidesRemoteTrash(t *testing.T) {
	tt := newTrashTest(t)
	tt.write(".Trash-1000/files/old.txt", "left by an older mount")
	tt.write("kept.txt", "x")
	entries, err := tt.vfs.ReadDir("")
	if err != nil {
		t.Fatal(err)
	}
	for _, e := range entries {
		if strings.HasPrefix(e.Name(), ".Trash") {
			t.Fatalf("listed %s", e.Name())
		}
	}
	if err := tt.vfs.Mkdir(tt.prefix, 0o700); err != nil {
		t.Fatal(err)
	}
	if !exists(tt.local("")) {
		t.Fatal("the trash folder wasn't made locally")
	}
}

func TestTrashFollowsRemote(t *testing.T) {
	tt := newTrashTest(t)
	tt.write("notes.txt", "draft")
	tt.cache("notes.txt")

	// Trashed in the web app.
	if err := os.Remove(filepath.Join(tt.remote, "notes.txt")); err != nil {
		t.Fatal(err)
	}
	if !tt.f.followRemote(tt.ctx, []string{"notes.txt"}, nil) {
		t.Fatal("nothing kept")
	}
	if got, _ := os.ReadFile(tt.local("files/notes.txt")); string(got) != "draft" {
		t.Fatalf("kept = %q", got)
	}
	info, _ := os.ReadFile(tt.local("info/notes.txt.trashinfo"))
	if !strings.Contains(string(info), "Path=notes.txt\n") {
		t.Fatalf(".trashinfo = %q", info)
	}
	if tt.f.followRemote(tt.ctx, []string{"notes.txt"}, nil) {
		t.Fatal("kept twice")
	}

	// Restored in the web app.
	tt.write("notes.txt", "draft")
	if !tt.f.followRemote(tt.ctx, []string{"notes.txt"}, nil) {
		t.Fatal("the entry wasn't dropped")
	}
	if exists(tt.local("files/notes.txt")) || exists(tt.local("info/notes.txt.trashinfo")) {
		t.Fatal("the entry is left")
	}
}

func TestTrashFollowsRemoteFolderTags(t *testing.T) {
	tt := newTrashTest(t)
	tt.write("Docs/letter.txt", "hello")
	tt.cache("Docs/letter.txt")

	// A WebDAV poll reports only the folder that changed.
	if err := os.Remove(filepath.Join(tt.remote, "Docs/letter.txt")); err != nil {
		t.Fatal(err)
	}
	if !tt.f.followRemote(tt.ctx, nil, []string{"Docs"}) {
		t.Fatal("nothing kept")
	}
	if !exists(tt.local("files/letter.txt")) {
		t.Fatal("the file wasn't kept")
	}
}

func TestTrashFollowsRemoteSkipsPendingUploads(t *testing.T) {
	tt := newTrashTest(t)
	// A file written into the mount that the VFS hasn't uploaded yet: in the cache, marked dirty, not on the remote.
	data, meta := tt.f.cachePaths("Drafts/new.txt")
	for _, p := range []string{data, meta} {
		if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
			t.Fatal(err)
		}
	}
	if err := os.WriteFile(data, []byte("new"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(meta, []byte(`{"Size":3,"Rs":[{"Pos":0,"Size":3}],"Dirty":true}`), 0o600); err != nil {
		t.Fatal(err)
	}
	tt.write("Drafts/other.txt", "x")

	if tt.f.followRemote(tt.ctx, []string{"Drafts/new.txt"}, []string{"Drafts"}) {
		t.Fatal("a pending upload was taken for a file that left the remote")
	}
	if exists(tt.local("files/new.txt")) {
		t.Fatal("the pending upload is in the trash")
	}
}
