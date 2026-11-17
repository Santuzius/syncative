package drive

// The listing cache on disk. Without it every start walked the whole remote tree again, which takes minutes on a large one; with it, a start only asks the event log what changed since the cursor saved with the listings. An expired cursor makes the first poll fail and the tree is walked as before.
//
// The file holds decrypted file and folder names, as the synced folders on the same device do; it is readable by the user only.

import (
	"encoding/json"
	"os"
	"path/filepath"
	"time"
)

const (
	// listingStoreVersion changes whenever the stored format or the meaning of its times changes; a file of another version is ignored.
	listingStoreVersion = 1
	// listingSaveDelay collects the changes of a few seconds into one write: a pass of uploads patches the cache after every file.
	listingSaveDelay = 10 * time.Second
)

// CacheDir is the folder for the listing caches of all sessions; empty keeps them in memory only. Set it once at startup, before any login or resume.
var CacheDir string

type storedListings struct {
	Version  int                 `json:"version"`
	VolumeID string              `json:"volume_id"`
	Cursor   string              `json:"cursor"`
	Dirs     map[string][]*Entry `json:"dirs"`
	Trees    map[string][]*Entry `json:"trees"`
}

// listingsPath is the session's cache file, "" when there is none.
func (s *Session) listingsPath() string {
	if CacheDir == "" || s.volumeID == "" {
		return ""
	}
	return filepath.Join(CacheDir, "proton-listings-"+s.volumeID+".json")
}

// loadListings fills the cache from disk. Called once the drive is bootstrapped (the volume ID names the file).
func (s *Session) loadListings() {
	path := s.listingsPath()
	if path == "" {
		return
	}
	data, err := os.ReadFile(path)
	if err != nil {
		if !os.IsNotExist(err) {
			logf("drive: could not read the cached listing: %v", err)
		}
		return
	}
	var stored storedListings
	if err := json.Unmarshal(data, &stored); err != nil || stored.Version != listingStoreVersion || stored.VolumeID != s.volumeID || stored.Cursor == "" {
		logf("drive: ignoring the cached listing on disk (other version or volume, or damaged).")
		return
	}
	l := &s.listings
	l.mu.Lock()
	defer l.mu.Unlock()
	l.cursor = stored.Cursor
	l.dirs = stored.Dirs
	l.trees = stored.Trees
	entries := 0
	for _, tree := range l.trees {
		entries += len(tree)
	}
	logf("drive: loaded the cached listing from disk (%d entries); the event log tells what changed since.", entries)
}

// markListingsDirty schedules a save of the cache. Caller holds mu.
func (s *Session) markListingsDirty() {
	l := &s.listings
	l.dirty = true
	if l.saveTimer != nil || l.closed || s.listingsPath() == "" {
		return
	}
	l.saveTimer = time.AfterFunc(listingSaveDelay, func() {
		l.mu.Lock()
		defer l.mu.Unlock()
		l.saveTimer = nil
		s.writeListings()
	})
}

// writeListings saves the cache, replacing the file atomically. Caller holds mu.
func (s *Session) writeListings() {
	l := &s.listings
	path := s.listingsPath()
	if !l.dirty || l.closed || path == "" {
		return
	}
	data, err := json.Marshal(storedListings{
		Version:  listingStoreVersion,
		VolumeID: s.volumeID,
		Cursor:   l.cursor,
		Dirs:     l.dirs,
		Trees:    l.trees,
	})
	if err == nil {
		err = os.MkdirAll(filepath.Dir(path), 0o700)
	}
	if err == nil {
		tmp := path + ".tmp"
		if err = os.WriteFile(tmp, data, 0o600); err == nil {
			err = os.Rename(tmp, path)
		}
	}
	if err != nil {
		logf("drive: could not save the cached listing: %v", err)
		return
	}
	l.dirty = false
}

// stopSavingListings ends the saves of a closing session, writing what is still pending; `forget` removes the file instead (sign-out).
func (s *Session) stopSavingListings(forget bool) {
	l := &s.listings
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.saveTimer != nil {
		l.saveTimer.Stop()
		l.saveTimer = nil
	}
	if !forget {
		s.writeListings()
	}
	l.closed = true
	if path := s.listingsPath(); forget && path != "" {
		_ = os.Remove(path)
	}
}
