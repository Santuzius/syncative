package rclone

// networkdrive.go: a network drive folder's pass asks what changed on the remote since the previous pass and has the mount's VFS forget those paths, so the mount follows the remote's sync interval and pause like a synced folder. rclone's own poll timer stays off (vfsOpt PollInterval 0). Backends with a change log (Google Drive, Dropbox, Proton) answer from it; on Nextcloud and ownCloud the folder ETags (davtags.go) lead down to the folders that changed.

import (
	"context"
	"errors"
	"fmt"
	"path"
	"strings"
	"sync"

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
	log, ok := f.(changeLog)
	if !ok {
		remote, root, _ := strings.Cut(fsString, ":")
		n, err := pollFolderTags(ctx, forget, fsString, remote, strings.Trim(root, "/"), state)
		if errors.Is(err, ErrNoFolderTags) {
			return rc.Params{"supported": false, "changes": 0}, nil
		}
		if err != nil {
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
	next, err := log.ChangesSince(ctx, func(path string, kind fs.EntryType) {
		key := "file"
		if kind == fs.EntryDirectory {
			key = "dir"
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
	return rc.Params{"supported": true, "changes": len(paths)}, nil
}

// pollFolderTags follows the folder ETags of a Nextcloud or ownCloud mount of folder `root` down to the folders that changed since the previous poll and has the VFS list those again; one request when nothing changed. Returns how many folders changed. The first poll only notes the tags.
func pollFolderTags(ctx context.Context, forget *rc.Call, fsString, remote, root string, state *mountState) (int, error) {
	params := rc.Params{"fs": fsString}
	// The VFS path of a folder, from the mount's root.
	rel := func(dir string) string {
		if dir == root {
			return ""
		}
		return strings.TrimPrefix(dir, root+"/")
	}
	first := len(state.tags) == 0
	changed := 0
	var walk func(dir, tag string) error
	walk = func(dir, known string) error {
		tag, children, err := FolderTags(ctx, remote, dir, 1)
		if err != nil {
			if dir != root && strings.Contains(err.Error(), "directory not found") {
				// Gone since its parent was read; the parent's new listing shows that.
				return nil
			}
			return err
		}
		state.tags[dir] = tag
		if tag == known {
			return nil
		}
		if !first {
			changed++
			// A file key invalidates the listing of the folder it lies in and keeps what the VFS knows below; the leaf needn't exist.
			params[fmt.Sprintf("file%d", len(params))] = path.Join(rel(dir), ".syncative-changed")
		}
		for child, childTag := range children {
			before, seen := state.tags[child]
			switch {
			case first:
				state.tags[child] = childTag
			case !seen:
				// Not seen before, so what changed below it is unknown: the VFS forgets all it knows there.
				state.tags[child] = childTag
				params[fmt.Sprintf("dir%d", len(params))] = rel(child)
			case before != childTag:
				if err := walk(child, before); err != nil {
					return err
				}
			}
		}
		return nil
	}
	if err := walk(root, state.tags[root]); err != nil {
		return 0, err
	}
	if len(params) > 1 {
		if _, err := forget.Fn(ctx, params); err != nil {
			return 0, err
		}
	}
	return changed, nil
}
