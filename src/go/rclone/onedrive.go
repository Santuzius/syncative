package rclone

// onedrive.go: OneDrive's change log (its delta listing) names items by ID, like Proton's. The fork's onedrive backend answers the query (github.com/Santuzius/rclone, backend/onedrive/syncative.go); this turns its answer into the itemChangeLog the sync asks (changes.go). A mount asks the backend's path-based ChangesSince directly (networkdrive.go).

import (
	"context"

	"github.com/rclone/rclone/backend/onedrive"
	"github.com/rclone/rclone/fs"
)

type onedriveLog struct{ f *onedrive.Fs }

func (l onedriveLog) ChangesRoot(ctx context.Context, dir string) (string, error) {
	return l.f.ChangesRoot(ctx, dir)
}

func (l onedriveLog) LatestItemEvent(ctx context.Context) (string, error) {
	return l.f.LatestItemEvent(ctx)
}

func (l onedriveLog) ItemChangesSince(ctx context.Context, event string) (ItemChanges, error) {
	next, relist, found, err := l.f.ItemChangesSince(ctx, event)
	if err != nil {
		return ItemChanges{}, err
	}
	items := make([]ItemChange, 0, len(found))
	for _, c := range found {
		items = append(items, ItemChange{
			ID:          c.ID,
			ParentID:    c.ParentID,
			Name:        c.Name,
			IsDir:       c.IsDir,
			ModTimeUnix: c.ModTime.Unix(),
			Gone:        c.Gone,
		})
	}
	return ItemChanges{Next: next, Relist: relist, Items: items}, nil
}

// itemLogOf is f's change log by ID, if it has one.
func itemLogOf(f fs.Fs) (itemChangeLog, bool) {
	if od, ok := f.(*onedrive.Fs); ok {
		return onedriveLog{od}, true
	}
	log, ok := f.(itemChangeLog)
	return log, ok
}
