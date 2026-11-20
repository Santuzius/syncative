package rclone

import (
	"testing"
	"time"
)

func TestWatcherHandsOverWhatChangedSinceACursor(t *testing.T) {
	w := &watcher{started: time.Now()}
	before := w.cursor()
	w.note(Changed{Path: "a/b.txt"})
	w.note(Changed{Path: "a/c", IsDir: true})
	changes, now, ok := w.since(before)
	if !ok || len(changes) != 2 || changes[1].Path != "a/c" || !changes[1].IsDir || now != w.cursor() {
		t.Fatalf("since: %v %q %v", changes, now, ok)
	}
	if changes, _, ok := w.since(now); !ok || len(changes) != 0 {
		t.Fatalf("since now: %v %v", changes, ok)
	}
	// A change the backend couldn't name, and a cursor of another watcher.
	w.note(Changed{unnamed: true})
	if _, _, ok := w.since(now); ok {
		t.Fatal("an unnamed change must not count as known")
	}
	other := &watcher{started: w.started.Add(time.Second)}
	if _, _, ok := other.since(before); ok {
		t.Fatal("another watcher's cursor must not count")
	}
}

func TestWatcherForgetsBeyondItsLog(t *testing.T) {
	w := &watcher{started: time.Now()}
	before := w.cursor()
	for range maxLogged + 1 {
		w.note(Changed{Path: "x"})
	}
	if _, _, ok := w.since(before); ok {
		t.Fatal("a cursor older than the log must not count")
	}
	recent := w.cursor()
	w.note(Changed{Path: "y"})
	if changes, _, ok := w.since(recent); !ok || len(changes) != 1 {
		t.Fatalf("recent: %v %v", changes, ok)
	}
}
