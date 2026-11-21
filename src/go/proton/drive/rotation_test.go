package drive

import (
	"testing"
	"time"
)

func TestNextRotationWaitsAndReportsEachSessionOnce(t *testing.T) {
	BindRemote("P", "uid-1")
	defer UnbindRemote("P")

	got := make(chan Rotation)
	go func() { got <- NextRotation() }()
	select {
	case r := <-got:
		t.Fatalf("returned %v before any renewal", r)
	case <-time.After(50 * time.Millisecond):
	}

	renewed("uid-1")
	renewed("uid-1")
	r := <-got
	if r.UID != "uid-1" || len(r.Remotes) != 1 || r.Remotes[0] != "P" {
		t.Fatalf("got %+v", r)
	}

	go func() { got <- NextRotation() }()
	select {
	case r := <-got:
		t.Fatalf("a second renewal of the same session before the call was reported again: %+v", r)
	case <-time.After(50 * time.Millisecond):
	}
	renewed("uid-2")
	if r := <-got; r.UID != "uid-2" || len(r.Remotes) != 0 {
		t.Fatalf("got %+v", r)
	}
}
