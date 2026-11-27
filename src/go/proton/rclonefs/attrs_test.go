package rclonefs

import (
	"context"
	"testing"

	"syncative/go/proton/drive"
)

// TestAttributesAreFetchedAgainAfterAnError: a file whose attributes couldn't be fetched (here: no session yet) gets them on a later call instead of keeping the error.
func TestAttributesAreFetchedAgainAfterAnError(t *testing.T) {
	o := &Object{fs: &Fs{name: "attrstest"}}
	if _, err := o.fileAttrs(context.Background()); err == nil {
		t.Fatal("no session, yet attributes")
	}

	drive.Register(&drive.Session{UID: "attrstest-uid"})
	drive.BindRemote("attrstest", "attrstest-uid")
	t.Cleanup(func() { drive.Unregister("attrstest-uid") })

	attrs, err := o.fileAttrs(context.Background())
	if err != nil || attrs == nil {
		t.Fatalf("after signing in: %v, %v", attrs, err)
	}
}
