package rclone

import (
	"context"
	"strings"
	"testing"

	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/fs/cache"
)

// fakeLog is a backend with a change log of numbered changes.
type fakeLog struct {
	fs.Fs
	name    string
	changes []string
}

func (f *fakeLog) Name() string { return f.name }
func (f *fakeLog) Root() string { return "" }

func (f *fakeLog) ChangesStartToken(context.Context) (string, error) {
	return "t" + strings.Repeat("x", len(f.changes)), nil
}

func (f *fakeLog) ChangesSince(_ context.Context, notify func(string, fs.EntryType), token string) (string, error) {
	for _, path := range f.changes[len(token)-1:] {
		notify(path, fs.EntryObject)
	}
	return "t" + strings.Repeat("x", len(f.changes)), nil
}

func TestChangesFollowTheLogUntilTheBackendIsForgotten(t *testing.T) {
	log := &fakeLog{name: "fake"}
	cache.Put("fake:", log)
	ctx := context.Background()

	first, err := ChangesSince(ctx, "fake", "")
	if err != nil || !first.Supported || first.Known {
		t.Fatalf("first: %+v %v", first, err)
	}
	log.changes = append(log.changes, "a/b.txt")
	next, err := ChangesSince(ctx, "fake", first.Cursor)
	if err != nil || !next.Known || len(next.Changes) != 1 || next.Changes[0].Path != "a/b.txt" {
		t.Fatalf("next: %+v %v", next, err)
	}
	if quiet, _ := ChangesSince(ctx, "fake", next.Cursor); !quiet.Known || len(quiet.Changes) != 0 {
		t.Fatalf("quiet: %+v", quiet)
	}

	// An earlier run's cursor, or one from before Forget, can't be followed.
	if old, _ := ChangesSince(ctx, "fake", "earlier.0:t"); old.Known {
		t.Fatalf("earlier run: %+v", old)
	}
	Forget("fake")
	if after, _ := ChangesSince(ctx, "fake", next.Cursor); after.Known {
		t.Fatalf("after Forget: %+v", after)
	}
}

// Two callers creating the same backend at once leave the later one in rclone's cache, which the listings then fill: cursors of the one used before can't be followed.
func TestChangesStartOverWhenTheCacheHoldsAnotherBackend(t *testing.T) {
	cache.Put("twice:", &fakeLog{name: "twice"})
	ctx := context.Background()
	first, err := ChangesSince(ctx, "twice", "")
	if err != nil || first.Known {
		t.Fatalf("first: %+v %v", first, err)
	}
	if same, _ := ChangesSince(ctx, "twice", first.Cursor); !same.Known {
		t.Fatalf("same backend: %+v", same)
	}

	cache.Put("twice:", &fakeLog{name: "twice"})
	if replaced, _ := ChangesSince(ctx, "twice", first.Cursor); replaced.Known {
		t.Fatalf("replaced backend: %+v", replaced)
	}
}

// renamingLog forgets a folder's old path once it has reported it, as Drive's backend does with its folder cache.
type renamingLog struct {
	fakeLog
	reported bool
}

func (f *renamingLog) ChangesSince(_ context.Context, notify func(string, fs.EntryType), token string) (string, error) {
	if token == "t" {
		if !f.reported {
			notify("Taxes", fs.EntryDirectory)
		}
		f.reported = true
		notify("Taxes-renamed", fs.EntryDirectory)
		return "tx", nil
	}
	return token, nil
}

// Every sync folder asking from the same cursor hears the same changes, though the backend could name an old path only once.
func TestEveryFolderHearsTheSameChanges(t *testing.T) {
	cache.Put("shared:", &renamingLog{fakeLog: fakeLog{name: "shared"}})
	ctx := context.Background()
	start, err := ChangesSince(ctx, "shared", "")
	if err != nil {
		t.Fatal(err)
	}
	for _, folder := range []string{"Notes", "Taxes"} {
		got, err := ChangesSince(ctx, "shared", start.Cursor)
		if err != nil || !got.Known || len(got.Changes) != 2 || got.Changes[0].Path != "Taxes" {
			t.Fatalf("%s: %+v %v", folder, got, err)
		}
	}
	// From the newer cursor nothing is left.
	later, _ := ChangesSince(ctx, "shared", strings.Replace(start.Cursor, ":t", ":tx", 1))
	if !later.Known || len(later.Changes) != 0 {
		t.Fatalf("later: %+v", later)
	}
}
