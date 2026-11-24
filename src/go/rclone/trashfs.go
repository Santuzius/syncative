package rclone

// trashfs.go: the backend a Linux network drive is mounted through, type "syncativetrash". It passes everything to the remote except the folder `.Trash-<uid>` at the mount's top, which a file manager following the freedesktop.org trash specification (Dolphin, through KIO) creates there to trash a file on the mount. That folder lives in a folder on this computer instead, so it never reaches the cloud, and a move into it trashes the file on the remote (trashbin.go). The remote's own `.Trash-*` folders, left by mounts before this backend, are hidden.
//
// The backend is named after the remote with a "{trash}" suffix, which no rclone remote name can contain: rclone's VFS keeps its cache under that name, and rclone's fs cache keeps the wrapper apart from the plain remote the sync folders use.

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path"
	"strings"
	"sync"
	"time"

	"github.com/rclone/rclone/backend/overview"
	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/fs/cache"
	"github.com/rclone/rclone/fs/config/configmap"
	"github.com/rclone/rclone/fs/config/configstruct"
	"github.com/rclone/rclone/fs/hash"
	"github.com/rclone/rclone/fs/object"

	// The trash folder's files are local files.
	_ "github.com/rclone/rclone/backend/local"
)

// TrashBackend is the type of the backend in connection strings, `:syncativetrash,remote=NAME,dir=PATH:FOLDER`.
const TrashBackend = "syncativetrash"

// trashNameSuffix marks the backend's name, see the file comment.
const trashNameSuffix = "{trash}"

func init() {
	fs.Register(&fs.RegInfo{
		Name:        TrashBackend,
		Description: "A remote mounted by Syncative, with the trash folder kept on this computer",
		NewFs:       newTrashFs,
		Hide:        true,
		// rclone's documentation data covers only its own backends.
		Overview: new(overview.BackendConfig),
		Options: []fs.Option{{
			Name:     "remote",
			Help:     "The rclone remote's name, without colon.",
			Required: true,
		}, {
			Name:     "dir",
			Help:     "The local folder for the mount's trash: its subfolder trash is .Trash-<uid>, entries what Syncative notes on each trashed file.",
			Required: true,
		}},
	})
}

type trashOptions struct {
	Remote string `config:"remote"`
	Dir    string `config:"dir"`
}

// trashFs is the backend, see the file comment.
type trashFs struct {
	name   string
	root   string
	remote string
	inner  fs.Fs
	local  fs.Fs
	// dir holds what the mount shows as `prefix`, `.Trash-<uid>`.
	dir    string
	prefix string
	// entries holds what Syncative knows of each trash entry, see trashEntry.
	entries  string
	features *fs.Features

	// mu guards dropped.
	mu sync.Mutex
	// dropped: trash entries whose file wasn't in the cache, by their name in files/; their .trashinfo is discarded when it arrives.
	dropped map[string]time.Time
}

func newTrashFs(ctx context.Context, name, root string, m configmap.Mapper) (fs.Fs, error) {
	opt := new(trashOptions)
	if err := configstruct.Set(m, opt); err != nil {
		return nil, err
	}
	if opt.Remote == "" || opt.Dir == "" {
		return nil, errors.New("syncativetrash needs remote and dir")
	}
	inner, err := cache.Get(ctx, opt.Remote+":"+root)
	if err != nil {
		if errors.Is(err, fs.ErrorIsFile) {
			return nil, fmt.Errorf("%s:%s is a file, not a folder", opt.Remote, root)
		}
		return nil, err
	}
	local, err := fs.NewFs(ctx, ":local:"+path.Join(opt.Dir, "trash"))
	if err != nil {
		return nil, err
	}
	f := &trashFs{
		name:    opt.Remote + trashNameSuffix,
		root:    inner.Root(),
		remote:  opt.Remote,
		inner:   inner,
		local:   local,
		dir:     path.Join(opt.Dir, "trash"),
		entries: path.Join(opt.Dir, "entries"),
		prefix:  fmt.Sprintf(".Trash-%d", os.Getuid()),
		dropped: map[string]time.Time{},
	}
	// The inner backend stays in rclone's cache, shared with the sync, as long as the wrapper lives.
	cache.PinUntilFinalized(inner, f)
	f.features = (&fs.Features{
		CanHaveEmptyDirectories: true,
	}).Fill(ctx, f).Mask(ctx, inner).WrapsFs(f, inner)
	// A move into the trash or out of it works on every remote; others are left to the remote and fall back to copying when it can't.
	f.features.Move = f.Move
	f.features.DirMove = f.DirMove
	return f, nil
}

// Name of the backend, see the file comment.
func (f *trashFs) Name() string { return f.name }

// Root is the remote's folder the mount shows.
func (f *trashFs) Root() string { return f.root }

func (f *trashFs) String() string { return fmt.Sprintf("%s with a local trash", f.inner) }

func (f *trashFs) Features() *fs.Features { return f.features }

func (f *trashFs) Precision() time.Duration { return f.inner.Precision() }

func (f *trashFs) Hashes() hash.Set { return f.inner.Hashes() }

// UnWrap returns the remote.
func (f *trashFs) UnWrap() fs.Fs { return f.inner }

// trashPath tells whether `remote` lies in the local trash folder, and where in it.
func (f *trashFs) trashPath(remote string) (rel string, ok bool) {
	if remote == f.prefix {
		return "", true
	}
	if rest, found := strings.CutPrefix(remote, f.prefix+"/"); found {
		return rest, true
	}
	return "", false
}

// hidden: `remote` lies in a trash folder on the remote, which the mount doesn't show; the local one takes the place of the remote's own `.Trash-<uid>`. Callers check trashPath first.
func (f *trashFs) hidden(remote string) bool {
	top, _, _ := strings.Cut(remote, "/")
	return strings.HasPrefix(top, ".Trash-")
}

// List lists folder `dir`.
func (f *trashFs) List(ctx context.Context, dir string) (fs.DirEntries, error) {
	if rel, ok := f.trashPath(dir); ok {
		entries, err := f.local.List(ctx, rel)
		if err != nil {
			return nil, err
		}
		out := make(fs.DirEntries, 0, len(entries))
		for _, entry := range entries {
			out = append(out, f.fromLocal(ctx, entry))
		}
		return out, nil
	}
	if f.hidden(dir) {
		return nil, fs.ErrorDirNotFound
	}
	entries, err := f.inner.List(ctx, dir)
	if err != nil {
		return nil, err
	}
	out := make(fs.DirEntries, 0, len(entries)+1)
	for _, entry := range entries {
		if dir == "" && f.hidden(entry.Remote()) {
			continue
		}
		if o, ok := entry.(fs.Object); ok {
			entry = f.wrap(o)
		}
		out = append(out, entry)
	}
	if dir == "" {
		if info, err := os.Stat(f.dir); err == nil && info.IsDir() {
			out = append(out, fs.NewDir(f.prefix, info.ModTime()))
		}
	}
	return out, nil
}

// NewObject finds file `remote`.
func (f *trashFs) NewObject(ctx context.Context, remote string) (fs.Object, error) {
	if rel, ok := f.trashPath(remote); ok {
		o, err := f.local.NewObject(ctx, rel)
		if err != nil {
			return nil, err
		}
		return f.trashObject(o), nil
	}
	if f.hidden(remote) {
		return nil, fs.ErrorObjectNotFound
	}
	o, err := f.inner.NewObject(ctx, remote)
	if err != nil {
		return nil, err
	}
	return f.wrap(o), nil
}

// Put uploads a file; in the trash folder it stays on this computer.
func (f *trashFs) Put(ctx context.Context, in io.Reader, src fs.ObjectInfo, options ...fs.OpenOption) (fs.Object, error) {
	if rel, ok := f.trashPath(src.Remote()); ok {
		return f.putTrash(ctx, in, src, rel, options...)
	}
	if f.hidden(src.Remote()) {
		return nil, fs.ErrorPermissionDenied
	}
	o, err := f.inner.Put(ctx, in, src, options...)
	if err != nil {
		return nil, err
	}
	return f.wrap(o), nil
}

// PutStream uploads a file of unknown size.
func (f *trashFs) PutStream(ctx context.Context, in io.Reader, src fs.ObjectInfo, options ...fs.OpenOption) (fs.Object, error) {
	if rel, ok := f.trashPath(src.Remote()); ok {
		return f.putTrash(ctx, in, src, rel, options...)
	}
	put := f.inner.Features().PutStream
	if put == nil || f.hidden(src.Remote()) {
		return nil, fs.ErrorPermissionDenied
	}
	o, err := put(ctx, in, src, options...)
	if err != nil {
		return nil, err
	}
	return f.wrap(o), nil
}

func (f *trashFs) putTrash(ctx context.Context, in io.Reader, src fs.ObjectInfo, rel string, options ...fs.OpenOption) (fs.Object, error) {
	if f.isDroppedInfo(rel) {
		// The .trashinfo of a file trashed without a copy here: nothing in the trash refers to it. The VFS checks what it uploaded, so the object returned holds the content; the VFS forgets it soon.
		content, err := io.ReadAll(in)
		if err != nil {
			return nil, err
		}
		f.forgetTrashSoon()
		o := object.NewMemoryObject(src.Remote(), src.ModTime(ctx), content)
		return &trashObject{Object: o, f: f, remote: src.Remote()}, nil
	}
	o, err := f.local.Put(ctx, in, fs.NewOverrideRemote(src, rel), options...)
	if err != nil {
		return nil, err
	}
	return f.trashObject(o), nil
}

// Mkdir makes folder `dir`.
func (f *trashFs) Mkdir(ctx context.Context, dir string) error {
	if rel, ok := f.trashPath(dir); ok {
		if rel == "" {
			// As KIO makes it: only its owner may enter it.
			return os.MkdirAll(f.dir, 0o700)
		}
		return f.local.Mkdir(ctx, rel)
	}
	if f.hidden(dir) {
		return fs.ErrorPermissionDenied
	}
	return f.inner.Mkdir(ctx, dir)
}

// Rmdir removes empty folder `dir`.
func (f *trashFs) Rmdir(ctx context.Context, dir string) error {
	if rel, ok := f.trashPath(dir); ok {
		if rel == "" {
			return os.Remove(f.dir)
		}
		return f.local.Rmdir(ctx, rel)
	}
	if f.hidden(dir) {
		return fs.ErrorDirNotFound
	}
	return f.inner.Rmdir(ctx, dir)
}

// Purge removes folder `dir` with all it holds; the VFS doesn't use it, rclone's other commands may.
func (f *trashFs) Purge(ctx context.Context, dir string) error {
	if rel, ok := f.trashPath(dir); ok {
		return os.RemoveAll(f.localPath(rel))
	}
	purge := f.inner.Features().Purge
	if purge == nil {
		return fs.ErrorCantPurge
	}
	return purge(ctx, dir)
}

// Copy copies a file on the remote, when the remote can.
func (f *trashFs) Copy(ctx context.Context, src fs.Object, remote string) (fs.Object, error) {
	copyFn := f.inner.Features().Copy
	inner, ok := src.(*wrappedObject)
	if copyFn == nil || !ok || f.hidden(remote) {
		return nil, fs.ErrorCantCopy
	}
	if _, inTrash := f.trashPath(remote); inTrash {
		return nil, fs.ErrorCantCopy
	}
	o, err := copyFn(ctx, inner.Object, remote)
	if err != nil {
		return nil, err
	}
	return f.wrap(o), nil
}

// Move moves file `src` to `remote`: within the remote, into the trash (trashing it), out of it (restoring it), or within the trash.
func (f *trashFs) Move(ctx context.Context, src fs.Object, remote string) (fs.Object, error) {
	dstRel, dstInTrash := f.trashPath(remote)
	switch s := src.(type) {
	case *trashObject:
		srcRel, _ := f.trashPath(s.remote)
		if dstInTrash {
			o, err := moveLocal(ctx, f.local, s.Object, dstRel)
			if err != nil {
				return nil, err
			}
			return f.trashObject(o), nil
		}
		return f.restoreFile(ctx, s, srcRel, remote)
	case *wrappedObject:
		if dstInTrash {
			return f.trashFile(ctx, s, dstRel)
		}
		move := f.inner.Features().Move
		if move == nil || f.hidden(remote) {
			return nil, fs.ErrorCantMove
		}
		o, err := move(ctx, s.Object, remote)
		if err != nil {
			return nil, err
		}
		return f.wrap(o), nil
	}
	return nil, fs.ErrorCantMove
}

// DirMove moves folder `srcRemote` to `dstRemote`, as Move does files.
func (f *trashFs) DirMove(ctx context.Context, src fs.Fs, srcRemote, dstRemote string) error {
	if src != fs.Fs(f) {
		return fs.ErrorCantDirMove
	}
	srcRel, srcInTrash := f.trashPath(srcRemote)
	dstRel, dstInTrash := f.trashPath(dstRemote)
	switch {
	case srcInTrash && dstInTrash:
		if srcRel == "" || dstRel == "" {
			return fs.ErrorCantDirMove
		}
		if _, err := os.Stat(f.localPath(dstRel)); err == nil {
			return fs.ErrorDirExists
		}
		if err := os.MkdirAll(path.Dir(f.localPath(dstRel)), 0o700); err != nil {
			return err
		}
		return os.Rename(f.localPath(srcRel), f.localPath(dstRel))
	case dstInTrash:
		if f.hidden(srcRemote) {
			return fs.ErrorCantDirMove
		}
		return f.trashDir(ctx, srcRemote, dstRel)
	case srcInTrash:
		if srcRel == "" || f.hidden(dstRemote) {
			return fs.ErrorCantDirMove
		}
		return f.restoreDir(ctx, srcRel, dstRemote)
	}
	move := f.inner.Features().DirMove
	if move == nil || f.hidden(srcRemote) || f.hidden(dstRemote) {
		return fs.ErrorCantDirMove
	}
	return move(ctx, f.inner, srcRemote, dstRemote)
}

// About reports the remote's storage.
func (f *trashFs) About(ctx context.Context) (*fs.Usage, error) {
	about := f.inner.Features().About
	if about == nil {
		return nil, errors.New("the remote reports no storage use")
	}
	return about(ctx)
}

// DirCacheFlush drops the remote's directory cache.
func (f *trashFs) DirCacheFlush() {
	if flush := f.inner.Features().DirCacheFlush; flush != nil {
		flush()
	}
}

// localPath is the OS path of `rel` in the local trash folder.
func (f *trashFs) localPath(rel string) string {
	return path.Join(f.dir, rel)
}

// fromLocal turns an entry of the local trash folder into one of the mount.
func (f *trashFs) fromLocal(ctx context.Context, entry fs.DirEntry) fs.DirEntry {
	switch e := entry.(type) {
	case fs.Object:
		return f.trashObject(e)
	case fs.Directory:
		return fs.NewDirCopy(ctx, e).SetRemote(path.Join(f.prefix, e.Remote()))
	}
	return entry
}

func (f *trashFs) wrap(o fs.Object) fs.Object {
	return &wrappedObject{Object: o, f: f}
}

func (f *trashFs) trashObject(o fs.Object) fs.Object {
	return &trashObject{Object: o, f: f, remote: path.Join(f.prefix, o.Remote())}
}

// wrappedObject is a file on the remote, seen through the wrapper: rclone moves and copies on the server only between files of the same backend.
type wrappedObject struct {
	fs.Object
	f *trashFs
}

func (o *wrappedObject) Fs() fs.Info { return o.f }

// UnWrap returns the remote's file.
func (o *wrappedObject) UnWrap() fs.Object { return o.Object }

// ModTime is the time the file was last modified where the remote lists another one (see shownModTime): the mount shows it, and rclone's VFS compares it to the cached copy's, along with size and hash.
func (o *wrappedObject) ModTime(ctx context.Context) time.Time { return shownModTime(ctx, o.Object) }

// ID is the remote's ID of the file, if it has one.
func (o *wrappedObject) ID() string {
	if ider, ok := o.Object.(fs.IDer); ok {
		return ider.ID()
	}
	return ""
}

// MimeType of the file, if the remote knows it.
func (o *wrappedObject) MimeType(ctx context.Context) string {
	if typer, ok := o.Object.(fs.MimeTyper); ok {
		return typer.MimeType(ctx)
	}
	return ""
}

// trashObject is a file in the local trash folder, at `remote` in the mount.
type trashObject struct {
	fs.Object
	f      *trashFs
	remote string
}

func (o *trashObject) Fs() fs.Info { return o.f }

func (o *trashObject) Remote() string { return o.remote }

func (o *trashObject) String() string { return o.remote }

// UnWrap returns the local file.
func (o *trashObject) UnWrap() fs.Object { return o.Object }

var (
	_ fs.Fs              = (*trashFs)(nil)
	_ fs.Mover           = (*trashFs)(nil)
	_ fs.DirMover        = (*trashFs)(nil)
	_ fs.Copier          = (*trashFs)(nil)
	_ fs.Purger          = (*trashFs)(nil)
	_ fs.PutStreamer     = (*trashFs)(nil)
	_ fs.Abouter         = (*trashFs)(nil)
	_ fs.UnWrapper       = (*trashFs)(nil)
	_ fs.DirCacheFlusher = (*trashFs)(nil)
	_ fs.ObjectUnWrapper = (*wrappedObject)(nil)
	_ fs.IDer            = (*wrappedObject)(nil)
	_ fs.ObjectUnWrapper = (*trashObject)(nil)
)
