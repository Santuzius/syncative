package main

/*
#include <stdlib.h>

struct RcloneRPCResult {
	char*	Output;
	int	Status;
};
*/
import "C"

import (
	"encoding/json"
	"unsafe"

	"syncative/go/rclone"

	"github.com/rclone/rclone/librclone/librclone"

	// Only the rclone backends Syncative can talk to: drive for Google Drive, dropbox, pcloud and webdav for remotes added by older versions, local for the copies between disk and remote, and alias for the Local folder remotes (a folder on this computer, beta). Every other backend (S3, Azure, …) only cost build time and binary size. Rclone's protondrive backend stays out in any case: it links henrybear327/go-proton-api, which collides with the ProtonMail/go-proton-api our own Drive layer (src/go/proton/drive/) uses.
	_ "github.com/rclone/rclone/backend/alias"
	_ "github.com/rclone/rclone/backend/drive"
	_ "github.com/rclone/rclone/backend/dropbox"
	_ "github.com/rclone/rclone/backend/local"
	_ "github.com/rclone/rclone/backend/onedrive"
	_ "github.com/rclone/rclone/backend/pcloud"
	_ "github.com/rclone/rclone/backend/webdav"

	// Proton Drive through Syncative's own client, see src/go/proton/rclonefs.
	_ "syncative/go/proton/rclonefs"

	_ "github.com/rclone/rclone/fs/operations"
	_ "github.com/rclone/rclone/fs/sync"
)

// These functions mirror upstream librclone-sys's C surface so any
// existing Rust caller (syncative::infrastructure::rclone) keeps working
// unchanged after we swap the crate dep.

//export RcloneInitialize
func RcloneInitialize() {
	librclone.Initialize()
}

//export RcloneFinalize
func RcloneFinalize() {
	librclone.Finalize()
}

// RcloneRPCResult mirrors the struct librclone-sys consumes. Must stay
// layout-compatible; the Rust side pattern-matches on it.
type RcloneRPCResult struct { //nolint:deadcode,unused
	Output *C.char
	Status C.int
}

//export RcloneRPC
func RcloneRPC(method *C.char, input *C.char) (result C.struct_RcloneRPCResult) {
	output, status := librclone.RPC(C.GoString(method), C.GoString(input))
	result.Output = C.CString(output)
	result.Status = C.int(status)
	return result
}

// RcloneRPCCancellable is RcloneRPC with a cancel flag: once the byte at `cancelled` turns non-zero, the call's context is cancelled, so rclone stops retrying and returns. Offline, a single Google Drive call otherwise retries for minutes. `cancelled` must stay valid until the call returns.
//
//export RcloneRPCCancellable
func RcloneRPCCancellable(method *C.char, input *C.char, cancelled *C.uchar) (result C.struct_RcloneRPCResult) {
	output, status := rclone.RPC(C.GoString(method), C.GoString(input), (*uint8)(unsafe.Pointer(cancelled)))
	result.Output = C.CString(output)
	result.Status = C.int(status)
	return result
}

// RcloneChanges asks the change log of the rclone remote `remote` (name without colon) what changed after `cursor` ("" for none) for the sync folder at `dir`; see rclone.ChangesSince. Returns JSON, `{"supported":…,"known":…,"cursor":…,"changes":[{"path":…,"is_dir":…}]}` (with `by_id`, `root_id` and `items` for Proton) or `{"error":…}`. Free the result with RcloneFreeString.
//
//export RcloneChanges
func RcloneChanges(remote *C.char, dir *C.char, cursor *C.char, cancelled *C.uchar) *C.char {
	ctx, stop := rclone.CancelledBy((*uint8)(unsafe.Pointer(cancelled)))
	defer stop()
	changes, err := rclone.ChangesSince(ctx, C.GoString(remote), C.GoString(dir), C.GoString(cursor))
	var out []byte
	if err != nil {
		out, _ = json.Marshal(map[string]string{"error": err.Error()})
	} else {
		out, _ = json.Marshal(changes)
	}
	return C.CString(string(out))
}

// RcloneFolderTags reads the change tags (ETags) of folder `path` and, with `depth` 1, of its subfolders on the WebDAV remote `remote`; see rclone.FolderTags. Returns JSON, `{"tag":…,"children":{path:tag}}` or `{"error":…}`. Free the result with RcloneFreeString.
//
//export RcloneFolderTags
func RcloneFolderTags(remote *C.char, path *C.char, depth C.int, cancelled *C.uchar) *C.char {
	ctx, stop := rclone.CancelledBy((*uint8)(unsafe.Pointer(cancelled)))
	defer stop()
	tag, children, err := rclone.FolderTags(ctx, C.GoString(remote), C.GoString(path), int(depth))
	var out []byte
	if err != nil {
		out, _ = json.Marshal(map[string]string{"error": err.Error()})
	} else {
		if children == nil {
			// A nil map would be null, which the Rust side doesn't take for an empty map.
			children = map[string]string{}
		}
		out, _ = json.Marshal(map[string]any{"tag": tag, "children": children})
	}
	return C.CString(string(out))
}

// RcloneForgetRemote drops the backend kept for `remote`'s change log; see rclone.Forget.
//
//export RcloneForgetRemote
func RcloneForgetRemote(remote *C.char) {
	rclone.Forget(C.GoString(remote))
}

//export RcloneFreeString
func RcloneFreeString(str *C.char) {
	C.free(unsafe.Pointer(str))
}
