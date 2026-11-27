// Package duplicates moves extra copies of a file on Google Drive, which keeps several files under one name, to the trash. An upload that rclone starts again after a broken connection can leave a second file of the same content; the sync sees one name and one file, so deleting it there brings the other one to light.
package duplicates

import (
	"context"
	"fmt"
	"path"
	"sort"
	"strings"

	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/fs/cache"
	"github.com/rclone/rclone/fs/hash"
	"github.com/rclone/rclone/fs/rc"
	"github.com/rclone/rclone/fs/walk"
)

func init() {
	rc.Add(rc.Call{
		Path:  "syncative/duplicates",
		Fn:    rcDuplicates,
		Title: "Move identical copies of files that share a name to the trash.",
		Help: `Parameters: fs (a Google Drive remote, "name:"), remote (the folder), recurse (whether to look in its subfolders too).

Among files that share a name in one folder, copies with the size and MD5 of the oldest one go to the Drive trash, where they can be restored until Drive empties it. Returns removed, the paths that had copies removed, and differing, the paths whose files differ and stay as they are.`,
	})
}

// Result is what Remove did.
type Result struct {
	// Removed are the paths that had identical copies removed.
	Removed []string `json:"removed"`
	// Differing are the paths whose files differ in content; all of them stay.
	Differing []string `json:"differing"`
}

func rcDuplicates(ctx context.Context, in rc.Params) (rc.Params, error) {
	name, err := in.GetString("fs")
	if err != nil {
		return nil, err
	}
	dir, err := in.GetString("remote")
	if err != nil {
		return nil, err
	}
	recurse, _ := in.GetBool("recurse")
	result, err := Remove(ctx, name, dir, recurse)
	if err != nil {
		return nil, err
	}
	return rc.Params{"removed": result.Removed, "differing": result.Differing}, nil
}

// Remove moves identical copies below `dir` of the Drive remote `name` ("GoogleDrive:"), in `dir` alone unless `recurse`, to the trash.
func Remove(ctx context.Context, name, dir string, recurse bool) (Result, error) {
	result := Result{Removed: []string{}, Differing: []string{}}
	// Removing goes to the trash unless the remote turned it off, so it is set on the Fs itself.
	f, err := cache.Get(ctx, strings.TrimSuffix(name, ":")+",use_trash=true:")
	if err != nil {
		return result, err
	}
	maxLevel := 1
	if recurse {
		maxLevel = -1
	}
	var objs []fs.Object
	err = walk.ListR(ctx, f, dir, true, maxLevel, walk.ListObjects, func(entries fs.DirEntries) error {
		entries.ForObject(func(obj fs.Object) { objs = append(objs, obj) })
		return nil
	})
	if err != nil {
		return result, err
	}
	for _, copies := range copiesOf(objs) {
		remote := copies[0].Remote()
		copies = oldestFirst(ctx, copies)
		if !identical(ctx, copies) {
			result.Differing = append(result.Differing, remote)
			continue
		}
		for _, obj := range copies[1:] {
			if err := obj.Remove(ctx); err != nil {
				return result, fmt.Errorf("removing a copy of %s: %w", remote, err)
			}
		}
		fs.Infof(nil, "%s: moved identical copies of %s to the trash (%d)", strings.TrimSuffix(name, ":"), remote, len(copies)-1)
		result.Removed = append(result.Removed, remote)
	}
	return result, nil
}

// copiesOf groups the files that share a name in one folder, sorted by path, leaving out files that are alone. Drive also keeps several folders under one name, whose files share a path without being copies of each other; and a file in two of them is listed twice under one ID, which must not be deleted as its own copy.
func copiesOf(objs []fs.Object) [][]fs.Object {
	type place struct{ parent, name string }
	byPlace := map[place][]fs.Object{}
	seen := map[string]bool{}
	for _, obj := range objs {
		if id := idOf(obj); id != "" {
			if seen[id] {
				continue
			}
			seen[id] = true
		}
		parent := path.Dir(obj.Remote())
		if p, ok := obj.(fs.ParentIDer); ok && p.ParentID() != "" {
			parent = p.ParentID()
		}
		at := place{parent, path.Base(obj.Remote())}
		byPlace[at] = append(byPlace[at], obj)
	}
	groups := [][]fs.Object{}
	for _, copies := range byPlace {
		if len(copies) > 1 {
			groups = append(groups, copies)
		}
	}
	sort.Slice(groups, func(i, j int) bool { return groups[i][0].Remote() < groups[j][0].Remote() })
	return groups
}

func idOf(obj fs.Object) string {
	if i, ok := obj.(fs.IDer); ok {
		return i.ID()
	}
	return ""
}

// oldestFirst sorts the copies by the time Drive created them, the first one kept; copies without that time go last.
func oldestFirst(ctx context.Context, objs []fs.Object) []fs.Object {
	created := make(map[fs.Object]string, len(objs))
	for _, obj := range objs {
		if m, err := fs.GetMetadata(ctx, obj); err == nil {
			created[obj] = m["btime"]
		}
	}
	sort.SliceStable(objs, func(i, j int) bool {
		a, b := created[objs[i]], created[objs[j]]
		if a == "" || b == "" {
			return a != ""
		}
		return a < b
	})
	return objs
}

// identical is true when every copy has the size and the MD5 of the first; a copy without an MD5, such as a Google Doc, counts as different.
func identical(ctx context.Context, objs []fs.Object) bool {
	first, err := objs[0].Hash(ctx, hash.MD5)
	if err != nil || first == "" {
		return false
	}
	for _, obj := range objs[1:] {
		sum, err := obj.Hash(ctx, hash.MD5)
		if err != nil || sum != first || obj.Size() != objs[0].Size() {
			return false
		}
	}
	return true
}
