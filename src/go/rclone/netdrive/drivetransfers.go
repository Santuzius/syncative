package netdrive

// drivetransfers.go tells which network drives move files right now, so their folders show it like a synced folder's pass. A drive downloads what the file manager opens and uploads what it saved, both inside rclone's VFS, outside any pass: rclone's transfer list names the files on their way and the VFS's write-back queue those waiting for their upload.

import (
	"context"
	"errors"
	"slices"

	"github.com/rclone/rclone/fs/accounting"
	"github.com/rclone/rclone/fs/cache"
	"github.com/rclone/rclone/fs/rc"
	"github.com/rclone/rclone/vfs/vfscache/writeback"
)

func init() {
	rc.Add(rc.Call{
		Path:  "syncative/drive-transfers",
		Fn:    rcDriveTransfers,
		Title: "Which files the network drives download and upload right now, and which finished.",
		Help:  `Parameters: fs, the drives' "remote:path" as mounted, a list. Returns drives: per fs with files on their way or finished, files, each with name (its path in the drive) and upload (false for a download); finished, the transfers among rclone's last completed ones, each with name, upload, error (empty when it went through) and at (when it ended, in nanoseconds since 1970); and moving, whether a file moves now rather than only waiting in the upload queue (as when offline).`,
	})
}

// driveFile is a file a network drive moves: a download, or an upload on its way or waiting in the VFS's write-back queue.
type driveFile struct {
	Name   string `json:"name"`
	Upload bool   `json:"upload"`
}

// finishedFile is a transfer of a network drive that ended, one try of an upload or a download.
type finishedFile struct {
	Name   string `json:"name"`
	Upload bool   `json:"upload"`
	Error  string `json:"error"`
	At     int64  `json:"at"`
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
	completed, err := rc.Calls.Get("core/transferred").Fn(ctx, rc.Params{})
	if err != nil {
		return nil, err
	}
	transferred, _ := completed["transferred"].([]accounting.TransferSnapshot)
	out := rc.Params{}
	for _, drive := range drives {
		// The VFS goes by the name rclone's cache gave the drive's backend.
		name := cache.Canonicalize(drive)
		if !slices.Contains(active, name) {
			continue
		}
		files := []driveFile{}
		for _, tr := range transferring {
			src, _ := tr["srcFs"].(string)
			dst, _ := tr["dstFs"].(string)
			file, _ := tr["name"].(string)
			if upload, ok := direction(src, dst, name); ok {
				files = addFile(files, driveFile{Name: file, Upload: upload})
			}
		}
		moving := len(files) > 0
		if queued, err := rc.Calls.Get("vfs/queue").Fn(ctx, rc.Params{"fs": name}); err == nil {
			items, _ := queued["queue"].([]writeback.QueueInfo)
			for _, item := range items {
				moving = moving || item.Uploading
				files = addFile(files, driveFile{Name: item.Name, Upload: true})
			}
		}
		finished := finishedFiles(transferred, name)
		if len(files) > 0 || len(finished) > 0 {
			out[drive] = rc.Params{"files": files, "finished": finished, "moving": moving}
		}
	}
	return rc.Params{"drives": out}, nil
}

// direction tells whether a transfer from `src` to `dst` belongs to the VFS `name`, and whether it is an upload: a download reads from its backend into the cache, which no transfer names; an upload writes to its backend from the cache. A synced folder's downloads write to a local folder, so they don't count.
func direction(src, dst, name string) (upload, ok bool) {
	switch {
	case src == name && dst == "":
		return false, true
	case dst == name && src != name:
		return true, true
	}
	return false, false
}

// addFile adds `file` to `files` unless it is there already.
func addFile(files []driveFile, file driveFile) []driveFile {
	if slices.Contains(files, file) {
		return files
	}
	return append(files, file)
}

// finishedFiles are the transfers of the VFS `name` among rclone's last completed ones. An upload rclone cancelled to start it again with the file's newer content didn't fail, so it is left out.
func finishedFiles(transferred []accounting.TransferSnapshot, name string) []finishedFile {
	finished := []finishedFile{}
	for _, tr := range transferred {
		upload, ok := direction(tr.SrcFs, tr.DstFs, name)
		if !ok || errors.Is(tr.Error, context.Canceled) {
			continue
		}
		file := finishedFile{Name: tr.Name, Upload: upload, At: tr.CompletedAt.UnixNano()}
		if tr.Error != nil {
			file.Error = tr.Error.Error()
		}
		finished = append(finished, file)
	}
	return finished
}
