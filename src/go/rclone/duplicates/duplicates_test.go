package duplicates

import (
	"context"
	"testing"

	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/fstest/mockobject"
)

func copyOf(content string) fs.Object {
	return mockobject.New("a.bin").WithContent([]byte(content), mockobject.SeekModeNone)
}

func TestOnlySameContentCountsAsIdentical(t *testing.T) {
	ctx := context.Background()
	if !identical(ctx, []fs.Object{copyOf("abc"), copyOf("abc"), copyOf("abc")}) {
		t.Error("three copies of the same bytes aren't identical")
	}
	if identical(ctx, []fs.Object{copyOf("abc"), copyOf("abd")}) {
		t.Error("files with different bytes count as identical")
	}
	if identical(ctx, []fs.Object{copyOf("abc"), copyOf("abcd")}) {
		t.Error("files of different sizes count as identical")
	}
	// A file without an MD5, such as a Google Doc, is never deleted as a copy.
	if identical(ctx, []fs.Object{mockobject.New("a.bin"), mockobject.New("a.bin")}) {
		t.Error("files without a hash count as identical")
	}
}
