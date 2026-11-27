// Package rclone holds Syncative's additions to rclone: RPC calls with a cancel flag (call.go), what changed on a remote (changes.go, with onedrive.go and davtags.go), the network drive (networkdrive.go, documents.go, drivetransfers.go, modtime.go) and its trash (trashfs.go, trashbin.go, nextcloudtrash.go).
package rclone

// changes.go asks a remote once per sync pass what changed since the previous one, so a pass can skip listing it; the sync folders of a remote share the answer. sourceOf tells how a backend reports changes, for the sync and for mounts (networkdrive.go) alike: the change logs of Google Drive and Dropbox name paths, those of OneDrive and Proton items by ID, and Nextcloud renews a folder's ETag with every change below it (davtags.go). The change-log calls come from Syncative's rclone fork (github.com/Santuzius/rclone, branch syncative), which exports the one-off query rclone's ChangeNotify otherwise runs on a timer.

import (
	"context"
	"errors"
	"fmt"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/fs/cache"
)

// changeLog is what the fork's drive and dropbox backends offer, and Syncative's Proton backend for mounts (networkdrive.go).
type changeLog interface {
	ChangesStartToken(ctx context.Context) (string, error)
	ChangesSince(ctx context.Context, notify func(string, fs.EntryType), token string) (string, error)
}

// Changed is one change the backend reported: the path of a file or folder, from the remote's root.
type Changed struct {
	Path  string `json:"path"`
	IsDir bool   `json:"is_dir"`
}

// ItemChange is one change of a log that names items by ID (Proton): the item as it is now.
type ItemChange struct {
	ID       string `json:"id"`
	ParentID string `json:"parent_id"`
	// Name in rclone's standard encoding, as listings give it.
	Name        string `json:"name,omitempty"`
	IsDir       bool   `json:"is_dir"`
	ModTimeUnix int64  `json:"mod_time_unix"`
	// Gone: trashed or deleted, with everything below it.
	Gone bool `json:"gone"`
}

// ItemChanges is what an itemChangeLog found after an event.
type ItemChanges struct {
	// Next is where the log stands after these changes.
	Next string
	// Relist: the log can't be followed from the given event; Next is then the latest one.
	Relist bool
	Items  []ItemChange
}

// itemChangeLog is what Syncative's Proton backend (syncative/go/proton/rclonefs) offers, and the fork's onedrive backend through onedriveLog: a log of items by ID, kept across restarts.
type itemChangeLog interface {
	// ChangesRoot returns the ID of folder `dir`; fs.ErrorDirNotFound when it is missing.
	ChangesRoot(ctx context.Context, dir string) (string, error)
	LatestItemEvent(ctx context.Context) (string, error)
	ItemChangesSince(ctx context.Context, event string) (ItemChanges, error)
}

// source is how a backend reports what changed; at most one of items and tags is set.
type source struct {
	// items: a log by ID, kept across restarts (OneDrive, Proton); the sync prefers it.
	items itemChangeLog
	// paths: a log by path (Google Drive, Dropbox; OneDrive and Proton too, which mounts use).
	paths changeLog
	// tags: folder ETags (Nextcloud).
	tags bool
}

// sourceOf tells how backend `f` of remote `remote` reports changes; the zero source when it can't.
func sourceOf(f fs.Fs, remote string) source {
	var s source
	s.items, _ = itemLogOf(f)
	s.paths, _ = f.(changeLog)
	if s.items == nil && s.paths == nil {
		_, err := nextcloudOptions(remote)
		s.tags = err == nil
	}
	return s
}

// Changes is what one query found.
type Changes struct {
	// Supported: the backend reports changes; without that the rest is empty.
	Supported bool `json:"supported"`
	// Known: Changes holds everything after the given cursor. False for a cursor of an earlier run or none; Cursor then starts from now ("" when the folder is missing).
	Known   bool      `json:"known"`
	Cursor  string    `json:"cursor"`
	Changes []Changed `json:"changes"`
	// Folders: list these folders again, and each subfolder whose ETag changed (Nextcloud).
	Folders []string `json:"folders,omitempty"`
	// ByID: the log names items by ID; Items replaces Changes, and RootID is the ID of the folder asked about.
	ByID   bool         `json:"by_id,omitempty"`
	RootID string       `json:"root_id,omitempty"`
	Items  []ItemChange `json:"items,omitempty"`
}

// run tells this process's cursors from those of an earlier one: Drive names a change's path through the folders its backend has listed, so after a restart (or Forget) the change log can't be followed until a listing has filled that cache again.
var run = strconv.FormatInt(time.Now().UnixNano(), 36)

var (
	mu sync.Mutex
	// pinned holds per remote the backend the last query used, kept in rclone's cache so the folders it has seen stay known between passes.
	pinned = map[string]fs.Fs{}
	// forgotten counts per remote how often its backend was dropped or replaced; the successor knows no folders yet.
	forgotten = map[string]int{}
	// answers holds per remote the change log's recent answers, see answerLog.
	answers = map[string]*answerLog{}
)

// maxAnswers bounds the answers kept per remote; a cursor older than all of them is asked of the provider directly.
const maxAnswers = 64

// answer is what the change log reported after one cursor, and the cursor after that.
type answer struct {
	next    string
	changes []Changed
	// items: the same for a log by ID, keyed by the event it starts from.
	items []ItemChange
}

// reuseFor is how long the change log's latest answer stands in for the provider. A remote's log covers all its sync folders, so the folders of one pass, which ask right after each other, need only one request. It is shorter than the shortest interval between passes (5 s), so each pass still asks at least once.
var reuseFor = 3 * time.Second

// answerLog keeps a remote's recent answers by the cursor they start from. Every sync folder of a remote asks with its own cursor, and asking the provider again for the same stretch of its log would not give the same answer: the first answer updates the backend's folder cache (a renamed folder leaves it), so a later one could no longer name a folder's old path. Its mutex lets one query at a time go to the provider.
type answerLog struct {
	sync.Mutex
	byCursor map[string]answer
	order    []string
	// tip is the cursor the provider last answered up to, at tipAt.
	tip   string
	tipAt time.Time
}

// current: the provider answered up to `cursor` less than reuseFor ago, so asking again can be skipped.
func (l *answerLog) current(cursor string) bool {
	return cursor == l.tip && time.Since(l.tipAt) < reuseFor
}

// asked records that the provider answered up to `cursor` just now.
func (l *answerLog) asked(cursor string) {
	l.tip, l.tipAt = cursor, time.Now()
}

func (l *answerLog) add(cursor string, a answer) {
	if _, ok := l.byCursor[cursor]; !ok {
		l.order = append(l.order, cursor)
	}
	l.byCursor[cursor] = a
	if len(l.order) > maxAnswers {
		delete(l.byCursor, l.order[0])
		l.order = l.order[1:]
	}
}

// answersOf returns `remote`'s answer log, an empty one after its backend changed.
func answersOf(remote string) *answerLog {
	mu.Lock()
	defer mu.Unlock()
	l, ok := answers[remote]
	if !ok {
		l = &answerLog{byCursor: map[string]answer{}}
		answers[remote] = l
	}
	return l
}

// cursorPrefix marks the cursors that the current backend of `remote` can follow.
func cursorPrefix(remote string) string {
	mu.Lock()
	defer mu.Unlock()
	return fmt.Sprintf("%s.%d:", run, forgotten[remote])
}

// backend returns the backend of `remote` that rclone's cache holds, the one the listings fill, created on first use. Cancelling `ctx` stops waiting for it to be created.
//
// Two callers asking for a backend that doesn't exist yet each create one, and the later replaces the earlier in the cache; so it is looked up on every query, and a different one than last time makes the cursors of its predecessor foreign.
func backend(ctx context.Context, remote string) (fs.Fs, error) {
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
	var f fs.Fs
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
	switch before, ok := pinned[remote]; {
	case !ok:
		// The pin belongs to the name, so it holds for every later backend of the remote too.
		cache.Pin(f)
	case before != f:
		forgotten[remote]++
		delete(answers, remote)
	}
	pinned[remote] = f
	return f, nil
}

// ChangesSince asks `remote` what changed after `cursor` (from an earlier call; "" for none) for the sync folder at `dir`.
func ChangesSince(ctx context.Context, remote, dir, cursor string) (Changes, error) {
	f, err := backend(ctx, remote)
	if err != nil {
		return Changes{}, err
	}
	switch s := sourceOf(f, remote); {
	case s.items != nil:
		return itemChangesSince(ctx, remote, s.items, dir, cursor)
	case s.paths != nil:
		return pathChangesSince(ctx, remote, s.paths, dir, cursor)
	case s.tags:
		return tagChangesSince(ctx, remote, dir, cursor)
	}
	return Changes{}, nil
}

// tagChangesSince is ChangesSince for Nextcloud: the cursor is the ETag of the folder at `dir`, which changes with everything below it. A missing folder gives no cursor; the listing tells what that means.
func tagChangesSince(ctx context.Context, remote, dir, cursor string) (Changes, error) {
	tag, _, err := FolderTags(ctx, remote, dir, 0)
	if err != nil && strings.Contains(err.Error(), "directory not found") {
		return Changes{Supported: true}, nil
	}
	if err != nil {
		return Changes{}, err
	}
	out := Changes{Supported: true, Known: cursor != "", Cursor: tag}
	if out.Known && tag != cursor {
		out.Folders = []string{dir}
	}
	return out, nil
}

// pathChangesSince is ChangesSince for a log by path (Google Drive, Dropbox). Its cursor holds the log's token behind the prefix of the backend that can follow it (cursorPrefix).
func pathChangesSince(ctx context.Context, remote string, log changeLog, dir, cursor string) (Changes, error) {
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
	l := answersOf(remote)
	l.Lock()
	defer l.Unlock()
	// Answers given to another sync folder cover the log from this cursor on as far as they reach; the provider is asked only for the rest.
	for range l.order {
		a, ok := l.byCursor[cursor]
		if !ok {
			break
		}
		out.Changes = append(out.Changes, a.changes...)
		cursor = a.next
	}
	if l.current(cursor) {
		out.Cursor = cursor
		return out, nil
	}
	token = strings.TrimPrefix(cursor, prefix)
	var fresh []Changed
	next, err := log.ChangesSince(ctx, func(path string, kind fs.EntryType) {
		fresh = append(fresh, Changed{Path: path, IsDir: kind == fs.EntryDirectory})
	}, token)
	if err != nil {
		return Changes{}, fmt.Errorf("change log: %w", err)
	}
	if next == "" {
		// The log gave no position to go on from: start over (without a cursor, which takes no lock).
		return pathChangesSince(ctx, remote, log, dir, "")
	}
	if next != token {
		l.add(cursor, answer{next: prefix + next, changes: fresh})
	}
	l.asked(prefix + next)
	out.Changes = append(out.Changes, fresh...)
	out.Cursor = prefix + next
	return out, nil
}

// itemChangesSince is ChangesSince for a log by ID (Proton, OneDrive). Its cursor is the last event seen and the ID of the folder at `dir`, so changes naming their folder by ID can be placed; it stays valid across restarts. The log covers the whole volume, so an answer given to one sync folder answers the others too while it is recent (reuseFor).
func itemChangesSince(ctx context.Context, remote string, log itemChangeLog, dir, cursor string) (Changes, error) {
	fresh := func(event string) (Changes, error) {
		root, err := log.ChangesRoot(ctx, dir)
		if errors.Is(err, fs.ErrorDirNotFound) {
			return Changes{Supported: true, ByID: true}, nil
		}
		if err != nil {
			return Changes{}, err
		}
		return Changes{Supported: true, ByID: true, Cursor: event + " " + root, RootID: root}, nil
	}
	event, root, ok := strings.Cut(cursor, " ")
	if !ok || event == "" || root == "" {
		// Before the listing, so a change during it shows up next time.
		latest, err := log.LatestItemEvent(ctx)
		if err != nil {
			return Changes{}, err
		}
		return fresh(latest)
	}
	l := answersOf(remote)
	l.Lock()
	defer l.Unlock()
	var items []ItemChange
	for range l.order {
		a, ok := l.byCursor[event]
		if !ok || a.items == nil {
			break
		}
		items = append(items, a.items...)
		event = a.next
	}
	if !l.current(event) {
		found, err := log.ItemChangesSince(ctx, event)
		if err != nil {
			return Changes{}, fmt.Errorf("change log: %w", err)
		}
		if found.Relist {
			return fresh(found.Next)
		}
		if len(found.Items) > 0 && found.Next != event {
			l.add(event, answer{next: found.Next, items: found.Items})
		}
		items = append(items, found.Items...)
		event = found.Next
		l.asked(event)
	}
	return Changes{Supported: true, Known: true, ByID: true, Cursor: event + " " + root, RootID: root, Items: items}, nil
}

// Forget drops `remote`'s backend, e.g. after its config was replaced or deleted; the next call creates it anew.
//
// rclone's own cache forgets the backends of a changed config (`cache.ClearConfig`), but not its names for them: a remote that wraps another, such as an alias onto a folder, leaves `Name:path` mapped to the wrapped path, and the next `Name:path` would still open the old folder. Dropping every name mapping is cheap: an unchanged remote is found again under its canonical name.
func Forget(remote string) {
	cache.ClearMappingsPrefix("")
	mu.Lock()
	defer mu.Unlock()
	if f, ok := pinned[remote]; ok {
		cache.Unpin(f)
		delete(pinned, remote)
	}
	forgotten[remote]++
	delete(answers, remote)
	forgetGate(remote)
}
