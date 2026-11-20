package drive

// The Drive volume's event log, as Syncative's sync follows it.
//
// Proton asks third-party clients to follow the volume's event log instead of re-listing the tree on a timer. Each event carries the changed link as it is now; ChangesSince turns the events after a cursor into that state with the decrypted name, and the sync puts them on top of what its database recorded (src/services/sync/remote_view.rs), so a pass costs one events request instead of a request per folder.

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"

	"github.com/ProtonMail/go-proton-api"

	"syncative/go/proton/extras"
)

// maxEventPages caps one call; a longer backlog makes the sync list instead, which is then cheaper than applying it.
const maxEventPages = 20

// volumeEvent is one entry of the volume's event log: the changed link as it is now.
type volumeEvent struct {
	EventType proton.LinkEventType
	Link      proton.Link
}

// Change is one link of the event log as it is now.
type Change struct {
	LinkID       string `json:"link_id"`
	ParentLinkID string `json:"parent_link_id"`
	Name         string `json:"name,omitempty"`
	IsDir        bool   `json:"is_dir"`
	ModTimeUnix  int64  `json:"mod_time_unix"`
	// Gone: trashed or deleted, with everything below it.
	Gone bool `json:"gone"`
}

// Changes is what ChangesSince found.
type Changes struct {
	// Cursor is where the event log stands after these changes.
	Cursor string `json:"cursor"`
	// Relist: the log can't be followed from the given cursor (expired, a refresh the server asked for, a backlog too long, an event that can't be read); Cursor is then the latest event, from which a fresh listing starts.
	Relist  bool     `json:"relist"`
	Changes []Change `json:"changes"`
}

// logf writes to stderr directly: librclone redirects the standard logger and hides anything below NOTICE.
func logf(format string, args ...any) {
	fmt.Fprintf(os.Stderr, format+"\n", args...)
}

// LatestEventID is the cursor of the volume's event log as it stands now.
func (s *Session) LatestEventID(ctx context.Context) (string, error) {
	return extras.LatestVolumeEventID(ctx, s.protonextAuth(), s.volumeID)
}

// ChangesSince returns the links that changed after event `cursor`, in the log's order. A request that fails on the way returns the error; one the server refuses means the cursor can't be followed, and the result asks for a listing.
func (s *Session) ChangesSince(ctx context.Context, cursor string) (*Changes, error) {
	auth := s.protonextAuth()
	relist := func(why string) (*Changes, error) {
		logf("drive: %s; listing again.", why)
		latest, err := s.LatestEventID(ctx)
		if err != nil {
			return nil, err
		}
		return &Changes{Cursor: latest, Relist: true}, nil
	}
	out := &Changes{Cursor: cursor}
	for range maxEventPages {
		page, err := extras.VolumeEvents(ctx, auth, s.volumeID, out.Cursor)
		if err != nil {
			var apiErr *proton.APIError
			if errors.As(err, &apiErr) && apiErr.Status < 500 && apiErr.Status != 429 {
				// An expired or unknown cursor.
				return relist(fmt.Sprintf("volume events: %v", err))
			}
			return nil, err
		}
		if page.Refresh {
			return relist("the server asked for a full refresh")
		}
		for _, raw := range page.Events {
			change, ok, err := s.change(ctx, raw)
			if err != nil {
				return relist(fmt.Sprintf("could not read a change from the volume event log (%v)", err))
			}
			if ok {
				out.Changes = append(out.Changes, change)
			}
		}
		if page.EventID != "" {
			out.Cursor = page.EventID
		}
		if !page.More {
			return out, nil
		}
	}
	return relist(fmt.Sprintf("more than %d pages of volume events", maxEventPages))
}

// change reads one event; false for a link no folder holds (the share's root).
func (s *Session) change(ctx context.Context, raw json.RawMessage) (Change, bool, error) {
	var ev volumeEvent
	if err := json.Unmarshal(raw, &ev); err != nil {
		return Change{}, false, err
	}
	link := ev.Link
	if link.LinkID == "" {
		return Change{}, false, fmt.Errorf("event %s names no link", string(raw[:min(len(raw), 80)]))
	}
	// Keyrings and metadata are fetched anew when next needed: a moved link's key is now encrypted to its new folder.
	delete(s.linkCache, link.LinkID)
	delete(s.krCache, link.LinkID)
	if ev.EventType == proton.LinkEventDelete || link.State != proton.LinkStateActive {
		return Change{LinkID: link.LinkID, ParentLinkID: link.ParentLinkID, Gone: true}, true, nil
	}
	if link.ParentLinkID == "" {
		return Change{}, false, nil
	}
	parentKR, err := s.linkKRByID(ctx, link.ParentLinkID)
	if err != nil {
		return Change{}, false, err
	}
	name, err := link.GetName(parentKR, s.defaultAddrKR)
	if err != nil {
		return Change{}, false, err
	}
	if link.Type == proton.LinkTypeFolder {
		s.linkCache[link.LinkID] = link
	}
	entry := entryFromLink(&link, name)
	return Change{LinkID: link.LinkID, ParentLinkID: link.ParentLinkID, Name: name, IsDir: entry.IsDir, ModTimeUnix: entry.ModTimeUnix}, true, nil
}
