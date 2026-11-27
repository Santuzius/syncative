// Package duplicates removes extra copies of a file on Google Drive, which keeps several files under one name. An upload that rclone starts again after a broken connection can leave a second file of the same content; the sync sees one name and one file, so deleting it there brings the other one to light.
package duplicates

import (
	"context"
	"fmt"
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
		Title: "Remove identical copies of files that share a name.",
		Help: `Parameters: fs (a Google Drive remote, "name:"), remote (the folder), recurse (whether to look in its subfolders too).

Among files with the same path, copies with the size and MD5 of the oldest one are deleted for good, past the trash, so they free their space at once. Returns removed, the paths that had copies removed, and differing, the paths whose files differ and stay as they are.`,
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

// Remove deletes identical copies below `dir` of the Drive remote `name` ("GoogleDrive:"), in `dir` alone unless `recurse`.
func Remove(ctx context.Context, name, dir string, recurse bool) (Result, error) {
	result := Result{Removed: []string{}, Differing: []string{}}
	// Deleting past the trash needs the backend option on the Fs itself.
	f, err := cache.Get(ctx, strings.TrimSuffix(name, ":")+",use_trash=false:")
	if err != nil {
		return result, err
	}
	maxLevel := 1
	if recurse {
		maxLevel = -1
	}
	byPath := map[string][]fs.Object{}
	err = walk.ListR(ctx, f, dir, true, maxLevel, walk.ListObjects, func(entries fs.DirEntries) error {
		for _, entry := range entries {
			if obj, ok := entry.(fs.Object); ok {
				byPath[obj.Remote()] = append(byPath[obj.Remote()], obj)
			}
		}
		return nil
	})
	if err != nil {
		return result, err
	}
	paths := make([]string, 0, len(byPath))
	for path, objs := range byPath {
		if len(objs) > 1 {
			paths = append(paths, path)
		}
	}
	sort.Strings(paths)
	for _, path := range paths {
		objs := oldestFirst(ctx, byPath[path])
		if !identical(ctx, objs) {
			result.Differing = append(result.Differing, path)
			continue
		}
		for _, obj := range objs[1:] {
			if err := obj.Remove(ctx); err != nil {
				return result, fmt.Errorf("removing a copy of %s: %w", path, err)
			}
		}
		fs.Infof(nil, "%s: deleted identical copies of %s (%d)", strings.TrimSuffix(name, ":"), path, len(objs)-1)
		result.Removed = append(result.Removed, path)
	}
	return result, nil
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
