package drive

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"testing"
)

func names(entries []*Entry) []string {
	out := make([]string, 0, len(entries))
	for _, e := range entries {
		out = append(out, e.Name)
	}
	return out
}

func treeCache() *listingCache {
	return &listingCache{
		cursor: "c1",
		dirs: map[string][]*Entry{
			"root": {{LinkID: "a", Name: "A", IsDir: true}},
			"a":    {{LinkID: "b", Name: "B", IsDir: true}, {LinkID: "f1", Name: "one.jpg"}},
		},
		trees: map[string][]*Entry{
			"root": {
				{LinkID: "a", Name: "A", IsDir: true},
				{LinkID: "b", Name: "A/B", IsDir: true},
				{LinkID: "f1", Name: "A/one.jpg"},
				{LinkID: "f2", Name: "A/B/two.jpg"},
			},
		},
	}
}

// treeNames lists the tree's paths sorted: patches append, and the order carries no meaning.
func treeNames(l *listingCache) string {
	out := names(l.trees["root"])
	sort.Strings(out)
	return fmt.Sprint(out)
}

func TestAnUploadIsPatchedIntoFolderAndTree(t *testing.T) {
	l := treeCache()
	before := l.trees["root"]
	_ = l.patchUpsert("b", &Entry{LinkID: "f3", Name: "three.jpg", ModTimeUnix: 5}, nil)
	_ = l.patchUpsert("a", &Entry{LinkID: "f1", Name: "one.jpg", ModTimeUnix: 9}, nil)

	if got := treeNames(l); got != "[A A/B A/B/three.jpg A/B/two.jpg A/one.jpg]" {
		t.Fatalf("tree: %s", got)
	}
	if got := fmt.Sprint(names(l.dirs["a"])); got != "[B one.jpg]" || l.dirs["a"][1].ModTimeUnix != 9 {
		t.Fatalf("folder: %s", got)
	}
	if len(before) != 4 || before[2].Name != "A/one.jpg" {
		t.Fatal("a listing handed out earlier was changed in place")
	}
}

func TestARenamedFolderTakesItsContentAlong(t *testing.T) {
	l := treeCache()
	_ = l.patchUpsert("a", &Entry{LinkID: "b", Name: "C", IsDir: true}, nil)
	if got := treeNames(l); got != "[A A/C A/C/two.jpg A/one.jpg]" {
		t.Fatalf("tree: %s", got)
	}
}

func TestAFolderMovedOutLeavesWithItsContent(t *testing.T) {
	l := treeCache()
	_ = l.patchUpsert("elsewhere", &Entry{LinkID: "b", Name: "B", IsDir: true}, nil)
	if got := treeNames(l); got != "[A A/one.jpg]" {
		t.Fatalf("tree: %s", got)
	}
	if got := fmt.Sprint(names(l.dirs["a"])); got != "[one.jpg]" {
		t.Fatalf("folder: %s", got)
	}
}

func TestAFolderMovedInIsListed(t *testing.T) {
	l := treeCache()
	list := func(linkID string) ([]*Entry, error) {
		if linkID != "m" {
			t.Fatalf("listed %s", linkID)
		}
		return []*Entry{{LinkID: "m1", Name: "in.jpg"}}, nil
	}
	if err := l.patchUpsert("b", &Entry{LinkID: "m", Name: "Moved", IsDir: true}, list); err != nil {
		t.Fatal(err)
	}
	if got := treeNames(l); got != "[A A/B A/B/Moved A/B/Moved/in.jpg A/B/two.jpg A/one.jpg]" {
		t.Fatalf("tree: %s", got)
	}
	failing := func(string) ([]*Entry, error) { return nil, errors.New("offline") }
	if err := l.patchUpsert("b", &Entry{LinkID: "n", Name: "Other", IsDir: true}, failing); err == nil {
		t.Fatal("a failed listing must surface")
	}
}

func TestADeletedFolderLeavesWithItsContent(t *testing.T) {
	l := treeCache()
	l.patchRemove("b")
	if got := treeNames(l); got != "[A A/one.jpg]" {
		t.Fatalf("tree: %s", got)
	}
}

func TestOnlyChangesNearCachedListingsConcernIt(t *testing.T) {
	l := treeCache()
	if !l.concerns("new", "b") || !l.concerns("f2", "elsewhere") {
		t.Fatal("a change in or out of a cached folder")
	}
	if l.concerns("x", "elsewhere") {
		t.Fatal("a change far away")
	}
}

func TestTheCacheSurvivesARestart(t *testing.T) {
	old := CacheDir
	CacheDir = t.TempDir()
	defer func() { CacheDir = old }()

	s := &Session{volumeID: "vol"}
	c := treeCache()
	s.listings.cursor, s.listings.dirs, s.listings.trees = c.cursor, c.dirs, c.trees
	s.listings.mu.Lock()
	s.listings.dirty = true
	s.writeListings()
	s.listings.mu.Unlock()
	if info, err := os.Stat(filepath.Join(CacheDir, "proton-listings-vol.json")); err != nil || info.Mode().Perm() != 0o600 {
		t.Fatalf("file: %v %v", info, err)
	}

	again := &Session{volumeID: "vol"}
	again.loadListings()
	if again.listings.cursor != "c1" || len(again.listings.trees["root"]) != 4 {
		t.Fatalf("loaded cursor %q, %d entries", again.listings.cursor, len(again.listings.trees["root"]))
	}
	other := &Session{volumeID: "other"}
	other.loadListings()
	if other.listings.cached() {
		t.Fatal("another volume's cache was loaded")
	}
}
