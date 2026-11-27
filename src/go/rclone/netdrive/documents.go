package netdrive

// documents.go: the network drive on Android. Android has no FUSE for apps, so Syncative's document provider (android/…/DocumentsProvider.java) shows the remote folder to other apps and works on the same kind of VFS a Linux mount uses, through the rc calls below and DocRead/DocWrite for the bytes. Each network drive folder has one VFS, by `remote:path`; syncative/mount-poll keeps its listings fresh as on Linux.

import (
	"context"
	"errors"
	"io"
	"os"
	"path"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/fs/cache"
	"github.com/rclone/rclone/fs/rc"
	"github.com/rclone/rclone/vfs"
	"github.com/rclone/rclone/vfs/vfscommon"
)

var docs = struct {
	sync.Mutex
	vfses   map[string]*vfs.VFS
	handles map[int64]vfs.Handle
	next    int64
}{vfses: map[string]*vfs.VFS{}, handles: map[int64]vfs.Handle{}}

func init() {
	for _, call := range []rc.Call{
		{Path: "syncative/doc-attach", Fn: rcDocAttach, Title: "Start the VFS of a network drive folder.", Help: `Parameters: fs, "remote:path". Returns started (bool): false when it ran already.`},
		{Path: "syncative/doc-detach", Fn: rcDocDetach, Title: "Stop the VFS of a network drive folder.", Help: `Parameters: fs.`},
		{Path: "syncative/doc-list", Fn: rcDocList, Title: "List a folder of a network drive.", Help: `Parameters: fs, path (relative, "" for the top). Returns entries: [{name, dir, size, modified (Unix ms)}].`},
		{Path: "syncative/doc-stat", Fn: rcDocStat, Title: "Describe a file or folder of a network drive.", Help: `Parameters: fs, path, probe (bool): a missing path then answers missing true instead of an error. Returns name, dir, size, modified.`},
		{Path: "syncative/doc-mkdir", Fn: rcDocMkdir, Title: "Create a folder in a network drive.", Help: `Parameters: fs, path.`},
		{Path: "syncative/doc-create", Fn: rcDocCreate, Title: "Create an empty file in a network drive.", Help: `Parameters: fs, path. Fails when the path exists. The file is listed at once and reaches the remote when it is first written and closed.`},
		{Path: "syncative/doc-remove", Fn: rcDocRemove, Title: "Delete a file or a folder with its content from a network drive.", Help: `Parameters: fs, path.`},
		{Path: "syncative/doc-rename", Fn: rcDocRename, Title: "Rename or move within a network drive.", Help: `Parameters: fs, from, to.`},
		{Path: "syncative/doc-open", Fn: rcDocOpen, Title: "Open a file of a network drive.", Help: `Parameters: fs, path, flags (os.O_* bits as Linux numbers them). Returns handle.`},
		{Path: "syncative/doc-size", Fn: rcDocSize, Title: "The size of an open file.", Help: `Parameters: handle.`},
		{Path: "syncative/doc-flush", Fn: rcDocFlush, Title: "Flush an open file.", Help: `Parameters: handle.`},
		{Path: "syncative/doc-close", Fn: rcDocClose, Title: "Close an open file; a written one is uploaded before this returns.", Help: `Parameters: handle.`},
	} {
		rc.Add(call)
	}
}

// docVFS returns the VFS for `remote:path`, starting it if needed, and whether it started now.
func docVFS(ctx context.Context, fsString string) (*vfs.VFS, bool, error) {
	docs.Lock()
	defer docs.Unlock()
	if v := docs.vfses[fsString]; v != nil {
		return v, false, nil
	}
	f, err := cache.Get(ctx, fsString)
	if err != nil {
		return nil, false, err
	}
	opt := vfscommon.Opt
	opt.CacheMode = vfscommon.CacheModeFull
	// No poll timer of rclone's own: each pass asks for the remote's changes (syncative/mount-poll).
	opt.PollInterval = 0
	// Files left unuploaded by an earlier process are queued when the VFS starts; with a write-back delay of 0 that would wait for their uploads here (as RSAF found, github.com/chenxiaolong/RSAF).
	opt.WriteBack = fs.Duration(time.Millisecond)
	v := vfs.New(context.Background(), f, &opt)
	// From now on Close uploads before it returns, so the provider knows when a file is up (and the foreground service keeps the process alive meanwhile).
	v.Opt.WriteBack = 0
	docs.vfses[fsString] = v
	return v, true, nil
}

func fsAndPath(ctx context.Context, in rc.Params, key string) (*vfs.VFS, string, error) {
	fsString, err := in.GetString("fs")
	if err != nil {
		return nil, "", err
	}
	p, err := in.GetString(key)
	if err != nil && !rc.IsErrParamNotFound(err) {
		return nil, "", err
	}
	v, _, err := docVFS(ctx, fsString)
	return v, strings.Trim(path.Clean("/"+p), "/"), err
}

func rcDocAttach(ctx context.Context, in rc.Params) (rc.Params, error) {
	fsString, err := in.GetString("fs")
	if err != nil {
		return nil, err
	}
	_, started, err := docVFS(ctx, fsString)
	return rc.Params{"started": started}, err
}

func rcDocDetach(ctx context.Context, in rc.Params) (rc.Params, error) {
	fsString, err := in.GetString("fs")
	if err != nil {
		return nil, err
	}
	docs.Lock()
	v := docs.vfses[fsString]
	delete(docs.vfses, fsString)
	docs.Unlock()
	if v != nil {
		v.Shutdown()
	}
	return rc.Params{}, nil
}

func entry(name string, info os.FileInfo) rc.Params {
	return rc.Params{
		"name":     name,
		"dir":      info.IsDir(),
		"size":     info.Size(),
		"modified": nodeModTime(info).UnixMilli(),
	}
}

func rcDocList(ctx context.Context, in rc.Params) (rc.Params, error) {
	v, p, err := fsAndPath(ctx, in, "path")
	if err != nil {
		return nil, err
	}
	node, err := v.Stat(p)
	if err != nil {
		return nil, err
	}
	dir, ok := node.(*vfs.Dir)
	if !ok {
		return nil, fs.ErrorIsFile
	}
	nodes, err := dir.ReadDirAll()
	if err != nil {
		return nil, err
	}
	entries := make([]rc.Params, 0, len(nodes))
	for _, n := range nodes {
		entries = append(entries, entry(n.Name(), n))
	}
	sort.Slice(entries, func(i, j int) bool { return entries[i]["name"].(string) < entries[j]["name"].(string) })
	return rc.Params{"entries": entries}, nil
}

func rcDocStat(ctx context.Context, in rc.Params) (rc.Params, error) {
	v, p, err := fsAndPath(ctx, in, "path")
	if err != nil {
		return nil, err
	}
	node, err := v.Stat(p)
	if errors.Is(err, vfs.ENOENT) {
		if probe, _ := in.GetBool("probe"); probe {
			return rc.Params{"missing": true}, nil
		}
	}
	if err != nil {
		return nil, err
	}
	return entry(node.Name(), node), nil
}

func rcDocMkdir(ctx context.Context, in rc.Params) (rc.Params, error) {
	v, p, err := fsAndPath(ctx, in, "path")
	if err != nil {
		return nil, err
	}
	return rc.Params{}, v.Mkdir(p, 0o777)
}

func rcDocCreate(ctx context.Context, in rc.Params) (rc.Params, error) {
	v, p, err := fsAndPath(ctx, in, "path")
	if err != nil {
		return nil, err
	}
	if _, err := v.Stat(p); err == nil {
		return nil, vfs.EEXIST
	}
	// Only listed for now: the app that created it opens it for writing next, and closing it then uploads it once. An empty file uploaded here would be a second upload (a second revision on Proton).
	return rc.Params{}, v.AddVirtual(p, 0, false)
}

func rcDocRemove(ctx context.Context, in rc.Params) (rc.Params, error) {
	v, p, err := fsAndPath(ctx, in, "path")
	if err != nil {
		return nil, err
	}
	if p == "" {
		return nil, vfs.EPERM
	}
	node, err := v.Stat(p)
	if err != nil {
		return nil, err
	}
	if dir, ok := node.(*vfs.Dir); ok {
		return rc.Params{}, dir.RemoveAll()
	}
	return rc.Params{}, node.Remove()
}

func rcDocRename(ctx context.Context, in rc.Params) (rc.Params, error) {
	v, from, err := fsAndPath(ctx, in, "from")
	if err != nil {
		return nil, err
	}
	to, err := in.GetString("to")
	if err != nil {
		return nil, err
	}
	to = strings.Trim(path.Clean("/"+to), "/")
	if from == "" || to == "" {
		return nil, vfs.EPERM
	}
	if _, err := v.Stat(to); err == nil {
		return nil, vfs.EEXIST
	}
	return rc.Params{}, v.Rename(from, to)
}

// Linux's open flags, as Android's OsConstants give them; Go's os.O_* are the same numbers on Linux, but the names keep it clear.
const (
	linuxWrOnly = 0o1
	linuxRdWr   = 0o2
	linuxCreat  = 0o100
	linuxTrunc  = 0o1000
	linuxAppend = 0o2000
)

func rcDocOpen(ctx context.Context, in rc.Params) (rc.Params, error) {
	v, p, err := fsAndPath(ctx, in, "path")
	if err != nil {
		return nil, err
	}
	bits, err := in.GetInt64("flags")
	if err != nil {
		return nil, err
	}
	flags := os.O_RDONLY
	switch {
	case bits&linuxRdWr != 0:
		flags = os.O_RDWR
	case bits&linuxWrOnly != 0:
		flags = os.O_WRONLY
	}
	if bits&linuxCreat != 0 {
		flags |= os.O_CREATE
	}
	if bits&linuxTrunc != 0 {
		flags |= os.O_TRUNC
	}
	if bits&linuxAppend != 0 {
		flags |= os.O_APPEND
	}
	h, err := v.OpenFile(p, flags, 0o666)
	if err != nil {
		return nil, err
	}
	docs.Lock()
	docs.next++
	id := docs.next
	docs.handles[id] = h
	docs.Unlock()
	return rc.Params{"handle": id}, nil
}

func handle(in rc.Params) (int64, vfs.Handle, error) {
	id, err := in.GetInt64("handle")
	if err != nil {
		return 0, nil, err
	}
	docs.Lock()
	defer docs.Unlock()
	h := docs.handles[id]
	if h == nil {
		return id, nil, vfs.EBADF
	}
	return id, h, nil
}

func rcDocSize(ctx context.Context, in rc.Params) (rc.Params, error) {
	_, h, err := handle(in)
	if err != nil {
		return nil, err
	}
	info, err := h.Stat()
	if err != nil {
		return nil, err
	}
	return rc.Params{"size": info.Size()}, nil
}

func rcDocFlush(ctx context.Context, in rc.Params) (rc.Params, error) {
	_, h, err := handle(in)
	if err != nil {
		return nil, err
	}
	return rc.Params{}, h.Flush()
}

func rcDocClose(ctx context.Context, in rc.Params) (rc.Params, error) {
	id, h, err := handle(in)
	if err != nil {
		return nil, err
	}
	docs.Lock()
	delete(docs.handles, id)
	docs.Unlock()
	return rc.Params{}, h.Close()
}

// ErrBadHandle: DocRead or DocWrite got a handle that isn't open.
var ErrBadHandle = errors.New("no such open file")

func openHandle(id int64) (vfs.Handle, error) {
	docs.Lock()
	defer docs.Unlock()
	if h := docs.handles[id]; h != nil {
		return h, nil
	}
	return nil, ErrBadHandle
}

// DocRead reads into `buf` from `offset` of the open file `id`, like pread: fewer bytes at the end of the file, 0 past it.
func DocRead(id int64, buf []byte, offset int64) (int, error) {
	h, err := openHandle(id)
	if err != nil {
		return 0, err
	}
	n, err := h.ReadAt(buf, offset)
	if errors.Is(err, io.EOF) {
		err = nil
	}
	return n, err
}

// DocWrite writes `buf` at `offset` of the open file `id`, like pwrite.
func DocWrite(id int64, buf []byte, offset int64) (int, error) {
	h, err := openHandle(id)
	if err != nil {
		return 0, err
	}
	return h.WriteAt(buf, offset)
}
