package rclone

// networkdrive.go: a network drive folder's pass asks the mounted backend's change log what changed since the previous pass and has the mount's VFS forget those paths, so the mount follows the remote's sync interval and pause like a synced folder. rclone's own poll timer stays off (vfsOpt PollInterval 0).

import (
	"context"
	"errors"
	"fmt"
	"sync"

	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/fs/cache"
	"github.com/rclone/rclone/fs/rc"
)

// mountTokens holds where each mount's change log stands: `remote:path` → token.
var mountTokens = struct {
	sync.Mutex
	byFs map[string]string
}{byFs: map[string]string{}}

func init() {
	rc.Add(rc.Call{
		Path:  "syncative/mount-poll",
		Fn:    rcMountPoll,
		Title: "Have a mount forget what changed on the remote since the last call.",
		Help:  `Parameters: fs, the mounted "remote:path"; reset (bool), true right after mounting, starts the change log anew. Returns supported (whether the backend has a change log) and changes (how many paths the mount forgot).`,
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
	log, ok := f.(changeLog)
	if !ok {
		return rc.Params{"supported": false, "changes": 0}, nil
	}
	forget := rc.Calls.Get("vfs/forget")
	if forget == nil {
		return nil, errors.New("this build has no VFS")
	}
	mountTokens.Lock()
	defer mountTokens.Unlock()
	token := mountTokens.byFs[fsString]
	if reset || token == "" {
		start, err := log.ChangesStartToken(ctx)
		if err != nil {
			return nil, err
		}
		mountTokens.byFs[fsString] = start
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
	mountTokens.byFs[fsString] = next
	return rc.Params{"supported": true, "changes": len(paths)}, nil
}
