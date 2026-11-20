// Package rclone holds Syncative's additions to rclone: RPC calls with a cancel flag (call.go), the change tags of WebDAV folders (davtags.go) and the change logs of Google Drive and Dropbox (changes.go).
package rclone

// changes.go asks Google Drive's and Dropbox's change logs once per sync pass what changed since the previous one, so a pass can skip listing the remote. The calls come from Syncative's rclone fork (github.com/Santuzius/rclone, branch syncative), which exports the one-off query rclone's ChangeNotify otherwise runs on a timer.

import (
	"context"
	"fmt"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/fs/cache"
)

// changeLog is what the fork's drive and dropbox backends offer.
type changeLog interface {
	ChangesStartToken(ctx context.Context) (string, error)
	ChangesSince(ctx context.Context, notify func(string, fs.EntryType), token string) (string, error)
}

// Changed is one change the backend reported: the path of a file or folder, from the remote's root.
type Changed struct {
	Path  string `json:"path"`
	IsDir bool   `json:"is_dir"`
}

// Changes is what one query found.
type Changes struct {
	// Supported: the backend has a change log; without one the rest is empty.
	Supported bool `json:"supported"`
	// Known: Changes holds everything after the given cursor. False for a cursor of an earlier run or none; Cursor then starts from now.
	Known   bool      `json:"known"`
	Cursor  string    `json:"cursor"`
	Changes []Changed `json:"changes"`
}

// run tells this process's cursors from those of an earlier one: Drive names a change's path through the folders its backend has listed, so after a restart (or Forget) the change log can't be followed until a listing has filled that cache again.
var run = strconv.FormatInt(time.Now().UnixNano(), 36)

var (
	mu sync.Mutex
	// pinned holds the backends kept in rclone's cache, so the folders they have seen stay known between passes.
	pinned = map[string]fs.Fs{}
	// forgotten counts per remote how often Forget dropped its backend, whose successor knows no folders yet.
	forgotten = map[string]int{}
)

// cursorPrefix marks the cursors that the current backend of `remote` can follow.
func cursorPrefix(remote string) string {
	mu.Lock()
	defer mu.Unlock()
	return fmt.Sprintf("%s.%d:", run, forgotten[remote])
}

// backend returns `remote`'s backend, created on first use and kept from then on. Cancelling `ctx` stops waiting for it to be created.
func backend(ctx context.Context, remote string) (fs.Fs, error) {
	mu.Lock()
	f, ok := pinned[remote]
	mu.Unlock()
	if ok {
		return f, nil
	}
	// rclone detaches the creation from the caller's context and, offline, retries for a long time; give up waiting on cancel instead. The creation finishes in the background and lands in rclone's cache.
	type created struct {
		f   fs.Fs
		err error
	}
	done := make(chan created, 1)
	go func() {
		f, err := cache.Get(context.Background(), remote+":")
		done <- created{f, err}
	}()
	select {
	case c := <-done:
		if c.err != nil {
			return nil, c.err
		}
		f = c.f
	case <-ctx.Done():
		return nil, ctx.Err()
	}
	mu.Lock()
	defer mu.Unlock()
	if existing, ok := pinned[remote]; ok {
		return existing, nil
	}
	cache.Pin(f)
	pinned[remote] = f
	return f, nil
}

// ChangesSince asks `remote`'s change log what changed after `cursor` (from an earlier call; "" for none).
func ChangesSince(ctx context.Context, remote, cursor string) (Changes, error) {
	f, err := backend(ctx, remote)
	if err != nil {
		return Changes{}, err
	}
	log, ok := f.(changeLog)
	if !ok {
		return Changes{}, nil
	}
	prefix := cursorPrefix(remote)
	token, ours := strings.CutPrefix(cursor, prefix)
	if !ours || token == "" {
		start, err := log.ChangesStartToken(ctx)
		if err != nil {
			return Changes{}, err
		}
		return Changes{Supported: true, Cursor: prefix + start}, nil
	}
	out := Changes{Supported: true, Known: true}
	next, err := log.ChangesSince(ctx, func(path string, kind fs.EntryType) {
		out.Changes = append(out.Changes, Changed{Path: path, IsDir: kind == fs.EntryDirectory})
	}, token)
	if err != nil {
		return Changes{}, fmt.Errorf("change log: %w", err)
	}
	if next == "" {
		// The log gave no position to go on from: start over.
		return ChangesSince(ctx, remote, "")
	}
	out.Cursor = prefix + next
	return out, nil
}

// Forget drops `remote`'s backend, e.g. after its config was replaced or deleted; the next call creates it anew.
func Forget(remote string) {
	mu.Lock()
	defer mu.Unlock()
	if f, ok := pinned[remote]; ok {
		cache.Unpin(f)
		delete(pinned, remote)
	}
	forgotten[remote]++
}
