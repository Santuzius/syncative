package main

import "C"

import (
	"unsafe"

	"syncative/go/rclone/netdrive"
)

// SyncativeDocRead reads up to `size` bytes at `offset` of the open network drive file `handle` (from the rc call syncative/doc-open) into `buf`. Returns how many, 0 at the end of the file, or -1 on an error.
//
//export SyncativeDocRead
func SyncativeDocRead(handle C.longlong, offset C.longlong, buf unsafe.Pointer, size C.int) C.int {
	n, err := netdrive.DocRead(int64(handle), unsafe.Slice((*byte)(buf), int(size)), int64(offset))
	if err != nil {
		return -1
	}
	return C.int(n)
}

// SyncativeDocWrite writes `size` bytes from `buf` at `offset` of the open network drive file `handle`. Returns how many, or -1 on an error.
//
//export SyncativeDocWrite
func SyncativeDocWrite(handle C.longlong, offset C.longlong, buf unsafe.Pointer, size C.int) C.int {
	n, err := netdrive.DocWrite(int64(handle), unsafe.Slice((*byte)(buf), int(size)), int64(offset))
	if err != nil {
		return -1
	}
	return C.int(n)
}
