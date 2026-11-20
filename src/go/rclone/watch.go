// Package rclone holds Syncative's additions to rclone: RPC calls with a cancel flag (call.go) and tracking of remote-side changes (watch.go).
//
// watch.go tracks remote-side changes of rclone remotes through the backend's change notification (Google Drive's and Dropbox's change logs), so a sync pass can tell that nothing changed since the previous one without listing the remote.
package rclone

import (
	"context"
	"fmt"
	"sync"
	"time"

	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/fs/cache"
)

const (
	// defaultPollInterval is how often the backend asks the provider's change log; one cheap request per remote.
	defaultPollInterval = 5 * time.Second
	// startupGrace gives no cursor for a while after watching starts: the backend fetches its first change-log cursor asynchronously, and changes before that cursor would go unnoticed.
	startupGrace = 3 * defaultPollInterval
)

// maxLogged bounds the changed paths a watcher keeps; a cursor older than that makes the sync list instead.
const maxLogged = 256

type watcher struct {
	started   time.Time
	intervals chan time.Duration // closing it stops the backend's poll loop

	mu sync.Mutex
	// generation counts the changes the backend reported; a cursor names one.
	generation uint64
	// log holds the changes after generation logStart, oldest first.
	log      []Changed
	logStart uint64
}

// Changed is one change the backend reported: the path of a file or folder, from the remote's root.
type Changed struct {
	Path  string `json:"path"`
	IsDir bool   `json:"is_dir"`
	// unnamed: something may have changed that the backend didn't report (see SetPollInterval).
	unnamed bool
}

func (w *watcher) note(c Changed) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.generation++
	w.log = append(w.log, c)
	if len(w.log) > maxLogged {
		w.log = w.log[1:]
		w.logStart++
	}
}

// cursor names the watcher's current state. The start time tells this watcher's cursors from those of an earlier one (an earlier run, or before Forget), whose change log ended with it.
func (w *watcher) cursor() string {
	w.mu.Lock()
	defer w.mu.Unlock()
	return fmt.Sprintf("%x:%d", w.started.UnixNano(), w.generation)
}

// since returns the changes after `cursor` and the cursor after them; false when they aren't all known.
func (w *watcher) since(cursor string) ([]Changed, string, bool) {
	var started int64
	var generation uint64
	if _, err := fmt.Sscanf(cursor, "%x:%d", &started, &generation); err != nil || started != w.started.UnixNano() {
		return nil, "", false
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	now := fmt.Sprintf("%x:%d", w.started.UnixNano(), w.generation)
	if generation < w.logStart || generation > w.generation {
		return nil, now, false
	}
	changes := w.log[generation-w.logStart:]
	for _, c := range changes {
		if c.unnamed {
			return nil, now, false
		}
	}
	return append([]Changed(nil), changes...), now, true
}

var (
	mu       sync.Mutex
	watchers = map[string]*watcher{} // nil value: backend without change notification
	// pollInterval is the current one; SetPollInterval changes it.
	pollInterval = defaultPollInterval
)

// SetPollInterval changes how often every watched remote, and every one watched from now on, asks its change log, e.g. less often while the device sleeps. Zero restores the default.
func SetPollInterval(d time.Duration) {
	if d <= 0 {
		d = defaultPollInterval
	}
	mu.Lock()
	defer mu.Unlock()
	shorter := d < pollInterval
	pollInterval = d
	for _, w := range watchers {
		if w == nil {
			continue
		}
		// The backend polls on its new rhythm only after the old wait; until then a change could go unnoticed by the pass that waking up starts.
		if shorter {
			w.note(Changed{unnamed: true})
		}
		// The backend reads the channel all the time; replace a value it has not picked up yet.
		select {
		case <-w.intervals:
		default:
		}
		select {
		case w.intervals <- d:
		default:
		}
	}
}

// Cursor names where `remote`'s change log stands: the same cursor later means nothing changed in between. "" when the backend has no change notification, or while watching has only just started and the backend may not yet have the change log's own starting point. The first call starts watching the remote. Cancelling `ctx` stops waiting for the backend to be created.
func Cursor(ctx context.Context, remote string) string {
	mu.Lock()
	w, ok := watchers[remote]
	mu.Unlock()
	if ok {
		if w == nil || time.Since(w.started) < startupGrace {
			return ""
		}
		return w.cursor()
	}

	// Without the lock: creating the backend may go over the network and, offline, retry for a long time, while SetPollInterval is called from Android's main thread. rclone detaches the creation from the caller's context, so give up waiting on cancel instead; the creation finishes in the background and lands in rclone's cache.
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
	var err error
	select {
	case c := <-done:
		f, err = c.f, c.err
	case <-ctx.Done():
		return ""
	}
	if err != nil {
		// Not recorded: try again on the next call.
		return ""
	}
	notify := f.Features().ChangeNotify

	mu.Lock()
	defer mu.Unlock()
	if _, ok := watchers[remote]; ok {
		// Another call started watching meanwhile.
		return ""
	}
	if notify == nil {
		watchers[remote] = nil
		return ""
	}
	w = &watcher{started: time.Now(), intervals: make(chan time.Duration, 1)}
	w.intervals <- pollInterval
	notify(context.Background(), func(path string, kind fs.EntryType) { w.note(Changed{Path: path, IsDir: kind == fs.EntryDirectory}) }, w.intervals)
	watchers[remote] = w
	return ""
}

// ChangesSince returns what `remote`'s backend reported after `cursor` (from Cursor) and the cursor after it; false when that isn't known completely: another watcher's cursor, more changes than are kept, or a change the backend couldn't name.
func ChangesSince(remote, cursor string) ([]Changed, string, bool) {
	mu.Lock()
	w := watchers[remote]
	mu.Unlock()
	if w == nil || time.Since(w.started) < startupGrace {
		return nil, "", false
	}
	return w.since(cursor)
}

// Forget stops watching `remote`, e.g. after its config was replaced or deleted. The next Changed call starts over.
func Forget(remote string) {
	mu.Lock()
	defer mu.Unlock()
	if w := watchers[remote]; w != nil {
		close(w.intervals)
	}
	delete(watchers, remote)
}
