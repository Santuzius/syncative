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

// driveFile is a file with the Drive ID and parent folder ID that the Drive backend gives its objects.
type driveFile struct {
	fs.Object
	id, parent string
}

func (d driveFile) ID() string       { return d.id }
func (d driveFile) ParentID() string { return d.parent }

func fileIn(remote, id, parent string) fs.Object {
	return driveFile{mockobject.New(remote), id, parent}
}

func TestOnlyFilesOfOneFolderAreCopies(t *testing.T) {
	groups := copiesOf([]fs.Object{
		fileIn("Docs/a.bin", "1", "docs"),
		fileIn("Docs/a.bin", "2", "docs"),
		// Two folders named "Same": their files share a path but no folder.
		fileIn("Same/b.bin", "3", "same-1"),
		fileIn("Same/b.bin", "4", "same-2"),
		// One file in both of them is listed twice under its ID.
		fileIn("Same/c.bin", "5", "same-1"),
		fileIn("Same/c.bin", "5", "same-1"),
	})
	if len(groups) != 1 || len(groups[0]) != 2 || groups[0][0].Remote() != "Docs/a.bin" {
		t.Errorf("expected only the two files in Docs as copies, got %v", groups)
	}
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
