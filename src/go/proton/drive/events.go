package drive

// Listing cache kept up to date by the Drive volume's event log.
//
// Proton asks third-party clients to follow the volume's event log instead of re-listing the tree on a timer. Every sync pass still asks for a full recursive listing, so the Session answers those from memory and applies the event log to them: each event carries the changed link as it is now, so a new or changed file or folder is put into the cached listings with its decrypted name, and a trashed or deleted one is taken out with everything below it. One cheap events request per pass replaces a ListChildren call per folder. Syncative's own uploads, new folders and deletes also patch the cache right away, so the rest of the pass sees them; their events later apply the same state again. The cache is emptied only when the server asks for a refresh, when the event log can't be read, or when an event can't be applied. It is kept on disk between runs (listing_store.go).

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"strings"
	"sync"
	"time"

	"github.com/ProtonMail/go-proton-api"

	"syncative/go/proton/extras"
)

const (
	// eventPollGap bounds how often the event log is asked. A sync pass resolves several paths and lists once; they all share one poll.
	eventPollGap = 5 * time.Second
	// maxEventPages caps one poll; a longer backlog empties the cache, as walking the tree is then cheaper than applying it.
	maxEventPages = 20
)

type listingCache struct {
	mu       sync.Mutex
	cursor   string // last event ID seen; empty until the first poll
	polledAt time.Time
	dirs     map[string][]*Entry // folder link ID → active children
	trees    map[string][]*Entry // root link ID → recursive listing

	// Saving to disk (listing_store.go).
	file      string
	dirty     bool
	saveTimer *time.Timer
	closed    bool
}

// volumeEvent is one entry of the volume's event log: the changed link as it is now.
type volumeEvent struct {
	EventType proton.LinkEventType
	Link      proton.Link
}

// logf writes to stderr directly: librclone redirects the standard logger and hides anything below NOTICE.
func logf(format string, args ...any) {
	fmt.Fprintf(os.Stderr, format+"\n", args...)
}

// cached reports whether any listing is held. Caller holds mu.
func (l *listingCache) cached() bool {
	return len(l.dirs) > 0 || len(l.trees) > 0
}

// clear drops every cached listing. Caller holds mu.
func (l *listingCache) clear() {
	if l.cached() {
		l.dirty = true
	}
	l.dirs = nil
	l.trees = nil
}

// invalidateListings forgets all cached listings, after a change of Syncative's own whose outcome is unknown (a request that failed on the way).
func (s *Session) invalidateListings() {
	s.listings.mu.Lock()
	defer s.listings.mu.Unlock()
	s.listings.clear()
	if s.listings.dirty {
		s.markListingsDirty()
	}
}

// patchUpsert puts `entry` (named by its base name) into the cached listings as a child of folder `parentLinkID`: it replaces the entry with the same link ID or name, takes a moved link out of its old folder and renames everything below a renamed or moved folder. `listFolder` lists a folder that newly enters a cached tree, whose content isn't cached yet; nil when the folder is known to be empty. The slices are replaced, never changed in place: a listing handed out earlier may still be serialised. Caller holds mu.
func (l *listingCache) patchUpsert(parentLinkID string, entry *Entry, listFolder func(linkID string) ([]*Entry, error)) error {
	for id, children := range l.dirs {
		if id != parentLinkID {
			l.dirs[id] = without(children, func(e *Entry) bool { return e.LinkID == entry.LinkID })
		}
	}
	if children, ok := l.dirs[parentLinkID]; ok {
		l.dirs[parentLinkID] = upserted(children, entry)
	}
	for rootID, tree := range l.trees {
		oldPath := ""
		for _, e := range tree {
			if e.LinkID == entry.LinkID {
				oldPath = e.Name
				break
			}
		}
		prefix, inTree := folderPath(tree, rootID, parentLinkID)
		if !inTree {
			if oldPath != "" {
				// Moved out of this tree.
				l.trees[rootID] = without(tree, func(e *Entry) bool { return e.Name == oldPath || strings.HasPrefix(e.Name, oldPath+"/") })
			}
			continue
		}
		e := *entry
		if prefix != "" {
			e.Name = prefix + "/" + entry.Name
		}
		tree = upserted(tree, &e)
		switch {
		case oldPath != "" && oldPath != e.Name:
			// Renamed or moved within the tree: everything below follows.
			tree = renamedBelow(tree, oldPath, e.Name)
		case oldPath == "" && e.IsDir && listFolder != nil:
			// A folder moved in from elsewhere brings content the cache has never seen.
			below, err := listFolder(e.LinkID)
			if err != nil {
				return err
			}
			for _, b := range below {
				moved := *b
				moved.Name = e.Name + "/" + b.Name
				tree = upserted(tree, &moved)
			}
		}
		l.trees[rootID] = tree
	}
	l.dirty = true
	return nil
}

// concerns reports whether a change of `linkID`, now in folder `parentLinkID`, touches a cached listing: the link is in one, or its folder is. Caller holds mu.
func (l *listingCache) concerns(linkID, parentLinkID string) bool {
	if _, ok := l.dirs[parentLinkID]; ok {
		return true
	}
	for rootID, tree := range l.trees {
		if _, ok := folderPath(tree, rootID, parentLinkID); ok {
			return true
		}
		for _, e := range tree {
			if e.LinkID == linkID {
				return true
			}
		}
	}
	for _, children := range l.dirs {
		for _, e := range children {
			if e.LinkID == linkID {
				return true
			}
		}
	}
	return false
}

// renamedBelow gives every entry below folder path `from` the path below `to`.
func renamedBelow(entries []*Entry, from, to string) []*Entry {
	out := make([]*Entry, 0, len(entries))
	for _, e := range entries {
		if strings.HasPrefix(e.Name, from+"/") {
			moved := *e
			moved.Name = to + strings.TrimPrefix(e.Name, from)
			e = &moved
		}
		out = append(out, e)
	}
	return out
}

// patchRemove drops a link, and for a folder everything below it, from the cached listings. Caller holds mu.
func (l *listingCache) patchRemove(linkID string) {
	for id, children := range l.dirs {
		l.dirs[id] = without(children, func(e *Entry) bool { return e.LinkID == linkID })
	}
	delete(l.dirs, linkID)
	delete(l.trees, linkID)
	for rootID, tree := range l.trees {
		var path string
		for _, e := range tree {
			if e.LinkID == linkID {
				path = e.Name
				break
			}
		}
		if path == "" {
			continue
		}
		l.trees[rootID] = without(tree, func(e *Entry) bool { return e.LinkID == linkID || strings.HasPrefix(e.Name, path+"/") })
	}
	l.dirty = true
}

// folderPath is the path of folder `linkID` within the recursive listing of `rootID`, "" for the root itself; false when the folder isn't part of it.
func folderPath(tree []*Entry, rootID, linkID string) (string, bool) {
	if linkID == rootID {
		return "", true
	}
	for _, e := range tree {
		if e.LinkID == linkID && e.IsDir {
			return e.Name, true
		}
	}
	return "", false
}

func upserted(entries []*Entry, entry *Entry) []*Entry {
	out := make([]*Entry, 0, len(entries)+1)
	for _, e := range entries {
		if e.LinkID != entry.LinkID && e.Name != entry.Name {
			out = append(out, e)
		}
	}
	return append(out, entry)
}

func without(entries []*Entry, drop func(*Entry) bool) []*Entry {
	out := make([]*Entry, 0, len(entries))
	for _, e := range entries {
		if !drop(e) {
			out = append(out, e)
		}
	}
	return out
}

// listingsChangedByUs patches the cache after Syncative changed `linkID` in folder `parentLinkID` itself: `entry` is its new state, nil when it is gone. Without an entry for a link that still exists, the cache is dropped.
func (s *Session) listingsChangedByUs(parentLinkID, linkID string, entry *Entry, gone bool) {
	l := &s.listings
	l.mu.Lock()
	defer l.mu.Unlock()
	if !l.cached() {
		return
	}
	switch {
	case gone:
		l.patchRemove(linkID)
	case entry != nil:
		// A new folder is empty, and a file has nothing below it.
		_ = l.patchUpsert(parentLinkID, entry, nil)
	default:
		l.clear()
	}
	s.markListingsDirty()
}

// applyEvent puts the link an event carries into the cached listings. Caller holds mu.
func (s *Session) applyEvent(ctx context.Context, raw json.RawMessage) error {
	var ev volumeEvent
	if err := json.Unmarshal(raw, &ev); err != nil {
		return err
	}
	link := ev.Link
	if link.LinkID == "" {
		return fmt.Errorf("event %s names no link", string(raw[:min(len(raw), 80)]))
	}
	// Keyrings and metadata are fetched anew when next needed.
	delete(s.linkCache, link.LinkID)
	delete(s.krCache, link.LinkID)
	if ev.EventType == proton.LinkEventDelete || link.State != proton.LinkStateActive {
		s.listings.patchRemove(link.LinkID)
		return nil
	}
	if link.ParentLinkID == "" || !s.listings.concerns(link.LinkID, link.ParentLinkID) {
		// The share's root, which no listing holds as a child, or a link outside every cached listing: nothing to decrypt.
		return nil
	}
	parentKR, err := s.linkKRByID(ctx, link.ParentLinkID)
	if err != nil {
		return err
	}
	name, err := link.GetName(parentKR, s.defaultAddrKR)
	if err != nil {
		return err
	}
	s.linkCache[link.LinkID] = link
	listFolder := func(linkID string) ([]*Entry, error) { return s.listRecursive(ctx, linkID) }
	return s.listings.patchUpsert(link.ParentLinkID, entryFromLink(&link, name), listFolder)
}

// pollEvents brings the cache up to date with the volume's event log. A refresh request from the server, an error or an event that can't be applied empties the cache; the caller then lists from the API. Caller holds mu.
func (s *Session) pollEvents(ctx context.Context) {
	l := &s.listings
	if time.Since(l.polledAt) < eventPollGap {
		return
	}
	l.polledAt = time.Now()
	cursorBefore := l.cursor
	defer func() {
		if l.dirty || l.cursor != cursorBefore {
			s.markListingsDirty()
		}
	}()

	auth := s.protonextAuth()
	if l.cursor == "" {
		id, err := extras.LatestVolumeEventID(ctx, auth, s.volumeID)
		if err != nil {
			logf("drive: latest volume event: %v", err)
			l.clear()
			return
		}
		// Nothing is cached before the first cursor, so there is nothing to apply.
		l.cursor = id
		return
	}

	applied := 0
	defer func() {
		if applied > 0 {
			logf("drive: applied %d change(s) from the volume event log to the cached listing.", applied)
		}
	}()
	for range maxEventPages {
		page, err := extras.VolumeEvents(ctx, auth, s.volumeID, l.cursor)
		if err != nil {
			// An expired or unknown cursor also lands here; start over from the latest event.
			logf("drive: volume events: %v; listing again.", err)
			l.clear()
			l.cursor = ""
			return
		}
		if page.Refresh {
			if l.cached() {
				logf("drive: the server asked for a full refresh; listing again.")
			}
			l.clear()
		} else if l.cached() {
			for _, raw := range page.Events {
				if err := s.applyEvent(ctx, raw); err != nil {
					logf("drive: could not apply a change from the volume event log (%v); listing again.", err)
					l.clear()
					break
				}
				applied++
			}
		}
		if page.EventID != "" {
			l.cursor = page.EventID
		}
		if !page.More {
			return
		}
	}
	logf("drive: more than %d pages of volume events; listing again.", maxEventPages)
	l.clear()
}

// cachedListing returns the cached listing of `linkID` if the event log shows no change since it was stored.
func (s *Session) cachedListing(ctx context.Context, recursive bool, linkID string) ([]*Entry, bool) {
	s.listings.mu.Lock()
	defer s.listings.mu.Unlock()
	s.pollEvents(ctx)
	m := s.listings.dirs
	if recursive {
		m = s.listings.trees
	}
	out, ok := m[linkID]
	return out, ok
}

// storeListing caches a fresh listing. A change landing between the poll and the listing shows up as an event on the next poll, which applies it (again).
func (s *Session) storeListing(recursive bool, linkID string, entries []*Entry) {
	l := &s.listings
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.cursor == "" {
		// No event cursor yet (the events request failed): there is no way to notice later changes, so don't cache.
		return
	}
	defer s.markListingsDirty()
	if recursive {
		if l.trees == nil {
			l.trees = make(map[string][]*Entry)
		}
		l.trees[linkID] = entries
	} else {
		if l.dirs == nil {
			l.dirs = make(map[string][]*Entry)
		}
		l.dirs[linkID] = entries
	}
}
