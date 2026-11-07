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
	"time"
	"unsafe"

	"syncative/go/rclone"

	"github.com/rclone/rclone/librclone/librclone"

	// Only the rclone backends Syncative can talk to: drive for Google Drive, dropbox, pcloud and webdav for remotes added by older versions, and local for the copies between disk and remote. Every other backend (S3, Azure, …) only cost build time and binary size. Rclone's protondrive backend stays out in any case: it links henrybear327/go-proton-api, which collides with the ProtonMail/go-proton-api our own Drive layer (src/go/proton/drive/) uses.
	_ "github.com/rclone/rclone/backend/drive"
	_ "github.com/rclone/rclone/backend/dropbox"
	_ "github.com/rclone/rclone/backend/local"
	_ "github.com/rclone/rclone/backend/pcloud"
	_ "github.com/rclone/rclone/backend/webdav"

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

// RcloneRemoteChanged reports (1/0) whether the rclone remote `remote` (name without colon) may have changed since the previous call; see rclone.Changed.
//
//export RcloneRemoteChanged
func RcloneRemoteChanged(remote *C.char, cancelled *C.uchar) C.int {
	ctx, stop := rclone.CancelledBy((*uint8)(unsafe.Pointer(cancelled)))
	defer stop()
	if rclone.Changed(ctx, C.GoString(remote)) {
		return 1
	}
	return 0
}

// RcloneForgetRemote stops change tracking for `remote`.
//
//export RcloneForgetRemote
func RcloneForgetRemote(remote *C.char) {
	rclone.Forget(C.GoString(remote))
}

// RcloneSetChangePollSeconds sets how often remotes with change notification ask their provider's change log; 0 restores the default. See rclone.SetPollInterval.
//
//export RcloneSetChangePollSeconds
func RcloneSetChangePollSeconds(seconds C.int) {
	rclone.SetPollInterval(time.Duration(seconds) * time.Second)
}

//export RcloneFreeString
func RcloneFreeString(str *C.char) {
	C.free(unsafe.Pointer(str))
}
