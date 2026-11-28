package netdrive

// drivetransfers.go tells which network drives move files right now, so their folders show it like a synced folder's pass. A drive downloads what the file manager opens and uploads what it saved, both inside rclone's VFS, outside any pass: rclone's transfer list names the files on their way and the VFS's write-back queue those waiting for their upload.

import (
	"context"
	"slices"

	"github.com/rclone/rclone/fs/cache"
	"github.com/rclone/rclone/fs/rc"
	"github.com/rclone/rclone/vfs/vfscache/writeback"
)

func init() {
	rc.Add(rc.Call{
		Path:  "syncative/drive-transfers",
		Fn:    rcDriveTransfers,
		Title: "Which files the network drives download and upload right now.",
		Help:  `Parameters: fs, the drives' "remote:path" as mounted, a list. Returns drives: per fs that moves files or has uploads waiting, files, each with name (its path in the drive), upload (false for a download) and failed (how often its upload failed so far), and moving, whether a file moves now rather than only waiting in the upload queue (as when offline).`,
	})
}

// driveFile is a file a network drive moves: a download, or an upload on its way or waiting in the VFS's write-back queue.
type driveFile struct {
	Name   string `json:"name"`
	Upload bool   `json:"upload"`
	Failed int    `json:"failed"`
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
		files := driveTransfers(transferring, name)
		moving := len(files) > 0
		if queued, err := rc.Calls.Get("vfs/queue").Fn(ctx, rc.Params{"fs": name}); err == nil {
			items, _ := queued["queue"].([]writeback.QueueInfo)
			files, moving = withQueue(files, items, moving)
		}
		if len(files) > 0 {
			out[drive] = rc.Params{"files": files, "moving": moving}
		}
	}
	return rc.Params{"drives": out}, nil
}

// driveTransfers lists the transfers of the VFS `name` in rclone's transfer list, each file once: a download reads from its backend into the cache, which no transfer names; an upload writes to its backend from the cache. A synced folder's downloads write to a local folder, so they don't count.
func driveTransfers(transferring []rc.Params, name string) []driveFile {
	files := []driveFile{}
	for _, tr := range transferring {
		src, _ := tr["srcFs"].(string)
		dst, _ := tr["dstFs"].(string)
		file, _ := tr["name"].(string)
		var upload bool
		switch {
		case src == name && dst == "":
		case dst == name && src != name:
			upload = true
		default:
			continue
		}
		if !slices.ContainsFunc(files, func(f driveFile) bool { return f.Name == file && f.Upload == upload }) {
			files = append(files, driveFile{Name: file, Upload: upload})
		}
	}
	return files
}

// withQueue adds the uploads waiting in the VFS's write-back queue to `files`, with how often each failed; one being uploaded counts as moving.
func withQueue(files []driveFile, queue []writeback.QueueInfo, moving bool) ([]driveFile, bool) {
	for _, item := range queue {
		moving = moving || item.Uploading
		// The queue counts the tries started, the one running too.
		failed := item.Tries
		if item.Uploading {
			failed--
		}
		i := slices.IndexFunc(files, func(f driveFile) bool { return f.Name == item.Name && f.Upload })
		if i < 0 {
			files = append(files, driveFile{Name: item.Name, Upload: true, Failed: failed})
		} else {
			files[i].Failed = failed
		}
	}
	return files, moving
}
