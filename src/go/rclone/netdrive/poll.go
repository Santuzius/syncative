package netdrive

// poll.go: a network drive folder's pass asks what changed on the remote since the previous pass and has the mount's VFS forget those paths, so the mount follows the remote's sync interval and pause like a synced folder. rclone's own poll timer stays off (vfsOpt PollInterval 0). Backends with a change log (Google Drive, Dropbox, OneDrive, Proton) answer from it; on Nextcloud the folder ETags (nextcloud/tags.go) lead down to the folders that changed.

import (
	"context"
	"errors"
	"fmt"
	"path"
	"strings"
	"sync"

	"syncative/go/rclone/changes"
	"syncative/go/rclone/nextcloud"

	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/fs/cache"
	"github.com/rclone/rclone/fs/rc"
)

// mounts holds what each mount's last poll saw, by `remote:path`.
var mounts = struct {
	sync.Mutex
	byFs map[string]*mountState
}{byFs: map[string]*mountState{}}

type mountState struct {
	// mu: one poll at a time per mount; others go on meanwhile.
	mu sync.Mutex
	// token: where the change log stands.
	token string
	// tags: the ETags seen of folders by path from the remote's root, for WebDAV.
	tags map[string]string
}

func init() {
	rc.Add(rc.Call{
		Path:  "syncative/mount-poll",
		Fn:    rcMountPoll,
		Title: "Have a mount forget what changed on the remote since the last call.",
		Help:  `Parameters: fs, the mounted "remote:path"; reset (bool), true right after mounting, starts anew. Returns supported (whether the backend has a change log or folder ETags) and changes (how many paths or folders changed).`,
	})
}

func rcMountPoll(ctx context.Context, in rc.Params) (rc.Params, error) {
	fsString, err := in.GetString("fs")
	if err != nil {
		return nil, err
	}
	reset, _ := in.GetBool("reset")
	// The VFS pins its backend in rclone's cache, so this is the mount's own, with the directory cache the change log reports through.
	f, err := cache.Get(ctx, fsString)
	if err != nil && !errors.Is(err, fs.ErrorIsFile) {
		return nil, err
	}
	// A Linux mount goes through the trash wrapper (trashfs.go); the change log is the remote's.
	trash, _ := f.(*trashFs)
	if trash != nil {
		f = trash.inner
	}
	forget := rc.Calls.Get("vfs/forget")
	if forget == nil {
		return nil, errors.New("this build has no VFS")
	}
	mounts.Lock()
	state := mounts.byFs[fsString]
	if state == nil || reset {
		state = &mountState{tags: map[string]string{}}
		mounts.byFs[fsString] = state
	}
	mounts.Unlock()
	state.mu.Lock()
	defer state.mu.Unlock()
	// After the VFS forgot what changed: files the cache holds that left the remote go into the local trash.
	followTrash := func(files, dirs []string) error {
		if trash == nil || !trash.followRemote(ctx, files, dirs) {
			return nil
		}
		_, err := forget.Fn(ctx, rc.Params{"fs": fsString, "dir": trash.prefix})
		return err
	}
	remote, root, _ := strings.Cut(fsString, ":")
	if trash != nil {
		remote, root = trash.remote, trash.root
	}
	// The VFS names what it forgets by path, so a mount follows a log by path even where the sync follows one by ID.
	src := changes.SourceOf(f, remote)
	log := src.Paths
	if log == nil {
		if !src.Tags {
			return rc.Params{"supported": false, "changes": 0}, nil
		}
		n, dirs, err := pollFolderTags(ctx, forget, fsString, remote, strings.Trim(root, "/"), state)
		if err != nil {
			return nil, err
		}
		if err := followTrash(nil, dirs); err != nil {
			return nil, err
		}
		return rc.Params{"supported": true, "changes": n}, nil
	}
	token := state.token
	if token == "" {
		start, err := log.ChangesStartToken(ctx)
		if err != nil {
			return nil, err
		}
		state.token = start
		return rc.Params{"supported": true, "changes": 0}, nil
	}
	paths := rc.Params{}
	var files, dirs []string
	next, err := log.ChangesSince(ctx, func(path string, kind fs.EntryType) {
		key := "file"
		if kind == fs.EntryDirectory {
			key = "dir"
			dirs = append(dirs, path)
		} else {
			files = append(files, path)
		}
		paths[fmt.Sprintf("%s%d", key, len(paths))] = path
	}, token)
	if err != nil {
		return nil, fmt.Errorf("change log: %w", err)
	}
	forgetAll := next == ""
	if forgetAll {
		// The log can't be followed from the token: the mount forgets everything (vfs/forget without paths), and the log starts anew.
		if next, err = log.ChangesStartToken(ctx); err != nil {
			return nil, err
		}
	}
	if forgetAll || len(paths) > 0 {
		params := rc.Params{"fs": fsString}
		if !forgetAll {
			for k, v := range paths {
				params[k] = v
			}
		}
		if _, err := forget.Fn(ctx, params); err != nil {
			return nil, err
		}
	}
	state.token = next
	if !forgetAll {
		if err := followTrash(files, dirs); err != nil {
			return nil, err
		}
	}
	return rc.Params{"supported": true, "changes": len(paths)}, nil
}

// pollFolderTags follows the folder ETags of a Nextcloud mount of folder `root` down to the folders that changed since the previous poll and has the VFS list those again; one request when nothing changed. Returns how many folders changed, and those and the folders new since then by their path in the mount. The first poll only notes the tags. The tags seen are kept only once the VFS forgot the changes, so a poll that fails finds them again next time.
func pollFolderTags(ctx context.Context, forget *rc.Call, fsString, remote, root string, state *mountState) (int, []string, error) {
	params := rc.Params{"fs": fsString}
	// The VFS path of a folder, from the mount's root.
	rel := func(dir string) string {
		if dir == root {
			return ""
		}
		return strings.TrimPrefix(dir, root+"/")
	}
	first := len(state.tags) == 0
	seen := map[string]string{}
	changed := 0
	var dirs []string
	var walk func(dir, tag string) error
	walk = func(dir, known string) error {
		tag, children, err := nextcloud.FolderTags(ctx, remote, dir, 1)
		if err != nil {
			if dir != root && errors.Is(err, fs.ErrorDirNotFound) {
				// Gone since its parent was read; the parent's new listing shows that.
				return nil
			}
			return err
		}
		seen[dir] = tag
		if tag == known {
			return nil
		}
		if !first {
			changed++
			dirs = append(dirs, rel(dir))
			// A file key invalidates the listing of the folder it lies in and keeps what the VFS knows below; the leaf needn't exist.
			params[fmt.Sprintf("file%d", len(params))] = path.Join(rel(dir), ".syncative-changed")
		}
		for child, childTag := range children {
			before, tracked := state.tags[child]
			switch {
			case first:
				seen[child] = childTag
			case !tracked:
				// Not seen before, so what changed below it is unknown: the VFS forgets all it knows there.
				seen[child] = childTag
				params[fmt.Sprintf("dir%d", len(params))] = rel(child)
				dirs = append(dirs, rel(child))
			case before != childTag:
				if err := walk(child, before); err != nil {
					return err
				}
			}
		}
		return nil
	}
	if err := walk(root, state.tags[root]); err != nil {
		return 0, nil, err
	}
	if len(params) > 1 {
		if _, err := forget.Fn(ctx, params); err != nil {
			return 0, nil, err
		}
	}
	for dir, tag := range seen {
		state.tags[dir] = tag
	}
	return changed, dirs, nil
}
