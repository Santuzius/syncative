package rclone

// drivetransfers.go tells which network drives move files right now, so their folders show it like a synced folder's pass. A drive downloads what the file manager opens and uploads what it saved, both inside rclone's VFS, outside any pass: rclone's transfer list names the files on their way and the VFS's write-back queue those waiting for their upload.

import (
	"context"
	"slices"

	"github.com/rclone/rclone/fs/cache"
	"github.com/rclone/rclone/fs/rc"
)

func init() {
	rc.Add(rc.Call{
		Path:  "syncative/drive-transfers",
		Fn:    rcDriveTransfers,
		Title: "How many files the network drives download and upload right now.",
		Help:  `Parameters: fs, the drives' "remote:path" as mounted, a list. Returns drives: per fs that moves files, downloading and uploading, how many files.`,
	})
}

func rcDriveTransfers(ctx context.Context, in rc.Params) (rc.Params, error) {
	var drives []string
	if err := in.GetStruct("fs", &drives); err != nil {
		return nil, err
	}
	list, err := rc.Calls.Get("vfs/list").Fn(ctx, rc.Params{})
	if err != nil {
		return nil, err
	}
	active, _ := list["vfses"].([]string)
	stats, err := rc.Calls.Get("core/stats").Fn(ctx, rc.Params{})
	if err != nil {
		return nil, err
	}
	transferring, _ := stats["transferring"].([]rc.Params)
	out := rc.Params{}
	for _, drive := range drives {
		// The VFS goes by the name rclone's cache gave the drive's backend.
		name := cache.Canonicalize(drive)
		if !slices.Contains(active, name) {
			continue
		}
		down, up := driveTransfers(transferring, name)
		if queued, err := rc.Calls.Get("vfs/stats").Fn(ctx, rc.Params{"fs": name}); err == nil {
			if disk, ok := queued["diskCache"].(rc.Params); ok {
				inProgress, _ := disk["uploadsInProgress"].(int)
				waiting, _ := disk["uploadsQueued"].(int)
				up = max(up, inProgress+waiting)
			}
		}
		if down > 0 || up > 0 {
			out[drive] = rc.Params{"downloading": down, "uploading": up}
		}
	}
	return rc.Params{"drives": out}, nil
}

// driveTransfers counts the transfers of the VFS `name` in rclone's transfer list: a download reads from its backend into the cache, which no transfer names; an upload writes to its backend from the cache. A synced folder's downloads write to a local folder, so they don't count.
func driveTransfers(transferring []rc.Params, name string) (down, up int) {
	for _, tr := range transferring {
		src, _ := tr["srcFs"].(string)
		dst, _ := tr["dstFs"].(string)
		switch {
		case src == name && dst == "":
			down++
		case dst == name && src != name:
			up++
		}
	}
	return down, up
}
