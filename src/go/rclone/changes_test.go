package rclone

import (
	"context"
	"strings"
	"testing"

	"github.com/rclone/rclone/fs"
)

// fakeLog is a backend with a change log of numbered changes.
type fakeLog struct {
	fs.Fs
	changes []string
}

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
	log := &fakeLog{}
	mu.Lock()
	pinned["fake"] = log
	mu.Unlock()
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
	mu.Lock()
	forgotten["fake"]++
	mu.Unlock()
	if after, _ := ChangesSince(ctx, "fake", next.Cursor); after.Known {
		t.Fatalf("after Forget: %+v", after)
	}
}
