package changes

// mega.go: MEGA's change log is the account's event stream, which go-mega follows to keep its node tree; it names nodes by handle. The fork's mega backend answers the query (github.com/Santuzius/rclone, backend/mega/syncative.go); this turns its answer into the ItemLog the sync asks (changes.go). A mount asks the backend's path-based ChangesSince directly (netdrive/poll.go).

import (
	"context"

	"github.com/rclone/rclone/backend/mega"
)

type megaLog struct{ f *mega.Fs }

func (l megaLog) ChangesRoot(ctx context.Context, dir string) (string, error) {
	return l.f.ChangesRoot(ctx, dir)
}

func (l megaLog) LatestItemEvent(ctx context.Context) (string, error) {
	return l.f.LatestItemEvent(ctx)
}

func (l megaLog) ItemChangesSince(ctx context.Context, event string) (ItemChanges, error) {
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
