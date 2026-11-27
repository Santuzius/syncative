package rclone

// trashbin.go: what the network drive's trash folder (trashfs.go) does with files moved in and out of it.
//
// - Trashed in the file manager: the file goes to the remote's own trash (Google Drive, Nextcloud and Proton delete into it). A copy the VFS cache holds in full is linked into the local trash, so the file manager's trash shows it and can restore it; a file not held in full leaves nothing here, and its .trashinfo is dropped. Nothing is downloaded for the trash.
// - Restored in the file manager: the remote takes the file out of its trash, keeping its history (Google Drive and Proton by the ID noted at trashing, Nextcloud by its trash bin's entry); when that fails, the local copy is uploaded.
// - Trashed or deleted on the remote: a mount poll (networkdrive.go) that sees a file in full in the cache gone from the remote links that copy into the local trash, with a .trashinfo. Restored there, the local entry is dropped.
//
// Emptying the trash or deleting from it in the file manager deletes only here; the remote's trash empties itself after its own time.

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"net/url"
	"os"
	"path"
	"path/filepath"
	"strings"
	"time"

	rfs "github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/fs/config"
	"github.com/rclone/rclone/fs/object"
	"github.com/rclone/rclone/fs/operations"
	"github.com/rclone/rclone/fs/rc"
	"github.com/rclone/rclone/lib/encoder"
	"github.com/rclone/rclone/lib/ranges"
)

// remoteTrash takes a trashed file or folder out of the remote's trash, back where it was. remoteTrashOf picks the way a remote has.
type remoteTrash interface {
	// restore returns errNotRestorable when `entry` lacks what this way needs.
	restore(ctx context.Context, entry trashEntry) error
}

// errNotRestorable: the remote's trash can't give back this entry; restoring uploads the local copy instead.
var errNotRestorable = errors.New("the remote has no trash to restore from")

// untrasher: a remote that takes a file or folder out of its trash by ID. Syncative's Proton backend and its Google Drive fork have it.
type untrasher interface {
	Untrash(ctx context.Context, id string) error
}

// idTrash restores by the ID noted at trashing.
type idTrash struct{ untrasher }

func (t idTrash) restore(ctx context.Context, entry trashEntry) error {
	if entry.ID == "" {
		return errNotRestorable
	}
	return t.Untrash(ctx, entry.ID)
}

// nextcloudTrash restores from Nextcloud's trash bin by where the entry was and when it went (nextcloudtrash.go); root is the mount's folder on the remote.
type nextcloudTrash struct{ remote, root string }

func (t nextcloudTrash) restore(ctx context.Context, entry trashEntry) error {
	return NextcloudRestore(ctx, t.remote, path.Join(t.root, entry.Path), entry.Deleted)
}

// remoteTrashOf is how backend `inner` of remote `remote`, mounted at folder `root`, restores from its trash; nil when it can't.
func remoteTrashOf(inner rfs.Fs, remote, root string) remoteTrash {
	if u, ok := inner.(untrasher); ok {
		return idTrash{u}
	}
	if _, err := nextcloudOptions(remote); err == nil {
		return nextcloudTrash{remote, root}
	}
	return nil
}

// trashEntry is what Syncative notes on an entry of the trash's files/, in entries/<name>.json.
type trashEntry struct {
	// ID: the remote's ID of the trashed file or folder, to take it out of the remote's trash; empty when the remote has none.
	ID string `json:"id,omitempty"`
	// Path: where it was, from the mount's top.
	Path string `json:"path"`
	Dir  bool   `json:"dir,omitempty"`
	// Deleted: when it went to the remote's trash; Nextcloud's trash bin is searched by it.
	Deleted time.Time `json:"deleted"`
	// OnRemote: it was trashed on the remote, not in the file manager.
	OnRemote bool `json:"on_remote,omitempty"`
}

// entryName: the name in files/ of trash path `rel` when `rel` is a whole entry, files/<name>.
func entryName(rel string) (string, bool) {
	name, ok := strings.CutPrefix(rel, "files/")
	if !ok || name == "" || strings.Contains(name, "/") {
		return "", false
	}
	return name, true
}

func (f *trashFs) entryPath(name string) string {
	return filepath.Join(f.entries, name+".json")
}

func (f *trashFs) readEntry(name string) (trashEntry, bool) {
	var entry trashEntry
	raw, err := os.ReadFile(f.entryPath(name))
	if err != nil || json.Unmarshal(raw, &entry) != nil {
		return trashEntry{}, false
	}
	return entry, true
}

func (f *trashFs) writeEntry(name string, entry trashEntry) {
	raw, _ := json.Marshal(entry)
	err := os.MkdirAll(f.entries, 0o700)
	if err == nil {
		err = os.WriteFile(f.entryPath(name), raw, 0o600)
	}
	if err != nil {
		rfs.Errorf(f, "couldn't note trash entry %q: %v", name, err)
	}
}

// removeEntry drops trash entry `name`: its file or folder, .trashinfo and note. After a restore in the file manager only the note is left to drop: the file manager deletes the .trashinfo itself.
func (f *trashFs) removeEntry(name string) {
	_ = os.RemoveAll(f.localPath("files/" + name))
	_ = os.Remove(f.localPath("info/" + name + ".trashinfo"))
	_ = os.Remove(f.entryPath(name))
}

// cachePaths: where the VFS keeps file `remote`'s data and metadata (vfs/vfscache/cache.go: `<cache>/vfs/<name>/<root>/<path>`).
func (f *trashFs) cachePaths(remote string) (data, meta string) {
	rel := strings.Trim(path.Join(f.name, f.root, remote), "/")
	osRel := filepath.FromSlash(encoder.OS.FromStandardPath(rel))
	base := config.GetCacheDir()
	return filepath.Join(base, "vfs", osRel), filepath.Join(base, "vfsMeta", osRel)
}

// cachedCopy: the VFS cache's copy of file `remote` when it holds all of it.
func (f *trashFs) cachedCopy(remote string) (string, bool) {
	data, _, ok := f.cachedFile(remote)
	return data, ok
}

// uploadedCopy: as cachedCopy, but only when the remote has the copy's content: a copy the VFS hasn't uploaded yet isn't missing from the remote because it left it.
func (f *trashFs) uploadedCopy(remote string) (string, bool) {
	data, dirty, ok := f.cachedFile(remote)
	return data, ok && !dirty
}

// cachedFile: where the VFS cache holds all of file `remote`, and whether it waits for upload.
func (f *trashFs) cachedFile(remote string) (data string, dirty, ok bool) {
	data, meta := f.cachePaths(remote)
	raw, err := os.ReadFile(meta)
	if err != nil {
		return "", false, false
	}
	// vfs/vfscache/item.go Info.
	var info struct {
		Size  int64
		Rs    ranges.Ranges
		Dirty bool
	}
	if json.Unmarshal(raw, &info) != nil {
		return "", false, false
	}
	st, err := os.Stat(data)
	if err != nil || !st.Mode().IsRegular() || st.Size() != info.Size {
		return "", false, false
	}
	if info.Size > 0 && !info.Rs.Present(ranges.Range{Pos: 0, Size: info.Size}) {
		return "", false, false
	}
	return data, info.Dirty, true
}

// keepCached links (or copies) the files of folder `remote` the cache holds in full into the local trash at `dst`; with `uploaded`, only those the remote has (uploadedCopy). Returns how many.
func (f *trashFs) keepCached(remote, dst string, uploaded bool) int {
	copyOf := f.cachedCopy
	if uploaded {
		copyOf = f.uploadedCopy
	}
	data, _ := f.cachePaths(remote)
	kept := 0
	_ = filepath.WalkDir(data, func(p string, d fs.DirEntry, err error) error {
		if err != nil || !d.Type().IsRegular() {
			return nil
		}
		rel, err := filepath.Rel(data, p)
		if err != nil {
			return nil
		}
		sub := encoder.OS.ToStandardPath(filepath.ToSlash(rel))
		if cached, ok := copyOf(path.Join(remote, sub)); ok && linkOrCopy(cached, filepath.Join(dst, filepath.FromSlash(sub))) == nil {
			kept++
		}
		return nil
	})
	return kept
}

// linkOrCopy makes `dst` a hard link of `src`, so the copy costs no space while the cache keeps its own, or a copy where the two lie on different file systems.
func linkOrCopy(src, dst string) error {
	if err := os.MkdirAll(filepath.Dir(dst), 0o700); err != nil {
		return err
	}
	if err := os.Link(src, dst); err == nil {
		return nil
	}
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()
	out, err := os.OpenFile(dst, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if err != nil {
		return err
	}
	if _, err := io.Copy(out, in); err != nil {
		out.Close()
		os.Remove(dst)
		return err
	}
	return out.Close()
}

// discarded stands for a file the trash doesn't keep.
func discarded(ctx context.Context, src rfs.ObjectInfo) rfs.Object {
	return object.NewMemoryObject(src.Remote(), src.ModTime(ctx), nil)
}

// drop: trash entry `name` keeps nothing here, so its .trashinfo goes too, now or once the VFS uploads it.
func (f *trashFs) drop(name string) {
	f.mu.Lock()
	for n, at := range f.dropped {
		if time.Since(at) > 10*time.Minute {
			delete(f.dropped, n)
		}
	}
	f.dropped[name] = time.Now()
	f.mu.Unlock()
	_ = os.Remove(f.localPath("info/" + name + ".trashinfo"))
	f.forgetTrashSoon()
}

// isDroppedInfo: trash path `rel` is the .trashinfo of a dropped entry; it is dropped as it arrives.
func (f *trashFs) isDroppedInfo(rel string) bool {
	name, ok := strings.CutPrefix(rel, "info/")
	if !ok {
		return false
	}
	name, ok = strings.CutSuffix(name, ".trashinfo")
	if !ok {
		return false
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	_, dropped := f.dropped[name]
	delete(f.dropped, name)
	return dropped
}

// forgetTrashSoon has the VFS list the trash folder anew once the move or upload that changed it behind the VFS's back has finished; the VFS still holds its locks meanwhile.
func (f *trashFs) forgetTrashSoon() {
	f.forgetSoon(f.prefix, time.Second)
}

// forgetSoon has the VFS list folder `dir` anew after each of `delays`, counted from now.
func (f *trashFs) forgetSoon(dir string, delays ...time.Duration) {
	go func() {
		start := time.Now()
		for _, delay := range delays {
			time.Sleep(time.Until(start.Add(delay)))
			if forget := rc.Calls.Get("vfs/forget"); forget != nil {
				_, _ = forget.Fn(context.Background(), rc.Params{"fs": rfs.ConfigString(f), "dir": dir})
			}
		}
	}()
}

// trashFile trashes file `src` on the remote, keeping a copy at `rel` in the local trash if the cache holds it in full.
func (f *trashFs) trashFile(ctx context.Context, src *wrappedObject, rel string) (rfs.Object, error) {
	name, whole := entryName(rel)
	dst := f.localPath(rel)
	cached, kept := f.cachedCopy(src.Remote())
	if kept {
		if err := linkOrCopy(cached, dst); err != nil {
			rfs.Errorf(src, "couldn't keep a copy in the trash: %v", err)
			kept = false
		}
	}
	id := src.ID()
	if err := src.Object.Remove(ctx); err != nil {
		if kept {
			_ = os.Remove(dst)
		}
		return nil, err
	}
	if !kept {
		if whole {
			f.drop(name)
		}
		return &trashObject{Object: discarded(ctx, src), f: f, remote: path.Join(f.prefix, rel)}, nil
	}
	if whole {
		f.writeEntry(name, trashEntry{ID: id, Path: src.Remote(), Deleted: time.Now()})
	}
	o, err := f.local.NewObject(ctx, rel)
	if err != nil {
		return nil, err
	}
	return f.trashObject(o), nil
}

// trashDir trashes folder `remote` on the remote, keeping at `rel` in the local trash the files of it the cache holds in full.
func (f *trashFs) trashDir(ctx context.Context, remote, rel string) error {
	name, whole := entryName(rel)
	dst := f.localPath(rel)
	id := f.dirID(ctx, remote)
	kept := f.keepCached(remote, dst, false)
	var err error
	if purge := f.inner.Features().Purge; purge != nil {
		err = purge(ctx, remote)
	} else {
		err = operations.Purge(ctx, f.inner, remote)
	}
	if err != nil {
		_ = os.RemoveAll(dst)
		return err
	}
	if kept == 0 {
		_ = os.RemoveAll(dst)
		if whole {
			f.drop(name)
		}
		return nil
	}
	if whole {
		f.writeEntry(name, trashEntry{ID: id, Path: remote, Dir: true, Deleted: time.Now()})
	}
	return nil
}

// dirID: the remote's ID of folder `remote`, from its parent's listing, or "" if it has none.
func (f *trashFs) dirID(ctx context.Context, remote string) string {
	parent := path.Dir(remote)
	if parent == "." {
		parent = ""
	}
	entries, err := f.inner.List(ctx, parent)
	if err != nil {
		return ""
	}
	for _, entry := range entries {
		if d, ok := entry.(rfs.Directory); ok && d.Remote() == remote {
			return d.ID()
		}
	}
	return ""
}

// untrash takes trash entry `entry` out of the remote's trash; errNotRestorable when the remote can't.
func (f *trashFs) untrash(ctx context.Context, entry trashEntry) error {
	t := remoteTrashOf(f.inner, f.remote, f.root)
	if t == nil {
		return errNotRestorable
	}
	return t.restore(ctx, entry)
}

// restoreFile restores trashed file `src` (at `rel` in the trash) to `remote`: from the remote's trash if it still has it, else by uploading the local copy.
func (f *trashFs) restoreFile(ctx context.Context, src *trashObject, rel, remote string) (rfs.Object, error) {
	name, whole := entryName(rel)
	if entry, ok := f.readEntry(name); whole && ok && f.free(ctx, entry.Path, remote) {
		o, err := f.untrashFile(ctx, entry, remote)
		if err == nil {
			_ = src.Object.Remove(ctx)
			_ = os.Remove(f.entryPath(name))
			return o, nil
		}
		rfs.Logf(src, "the remote's trash couldn't restore it, uploading the copy kept here: %v", err)
	}
	o, err := operations.Copy(ctx, f.inner, nil, remote, src.Object)
	if err != nil {
		return nil, err
	}
	_ = src.Object.Remove(ctx)
	if whole {
		_ = os.Remove(f.entryPath(name))
	}
	return f.wrap(o), nil
}

func (f *trashFs) untrashFile(ctx context.Context, entry trashEntry, remote string) (rfs.Object, error) {
	if err := f.untrash(ctx, entry); err != nil {
		return nil, err
	}
	f.DirCacheFlush()
	o, err := f.inner.NewObject(ctx, entry.Path)
	if err != nil {
		return nil, fmt.Errorf("not back at %s after the restore: %w", entry.Path, err)
	}
	if entry.Path != remote {
		if o, err = operations.Move(ctx, f.inner, nil, remote, o); err != nil {
			return nil, err
		}
	}
	return f.wrap(o), nil
}

// restoreDir restores trashed folder `rel` of the trash to `remote`, as restoreFile does a file.
func (f *trashFs) restoreDir(ctx context.Context, rel, remote string) error {
	name, whole := entryName(rel)
	if entry, ok := f.readEntry(name); whole && ok && f.free(ctx, entry.Path, remote) {
		err := f.untrashDir(ctx, entry, remote)
		if err == nil {
			_ = os.RemoveAll(f.localPath(rel))
			_ = os.Remove(f.entryPath(name))
			return nil
		}
		rfs.Logf(f, "the remote's trash couldn't restore %s, uploading the files kept here: %v", entry.Path, err)
	}
	if err := f.upload(ctx, rel, remote); err != nil {
		return err
	}
	_ = os.RemoveAll(f.localPath(rel))
	if whole {
		_ = os.Remove(f.entryPath(name))
	}
	return nil
}

func (f *trashFs) untrashDir(ctx context.Context, entry trashEntry, remote string) error {
	if err := f.untrash(ctx, entry); err != nil {
		return err
	}
	f.DirCacheFlush()
	// Google Drive lists the files of a restored folder only after some seconds; the VFS lists it anew then.
	f.forgetSoon(remote, 5*time.Second, 20*time.Second)
	if entry.Path == remote {
		return nil
	}
	return operations.DirMove(ctx, f.inner, entry.Path, remote)
}

// upload copies folder `rel` of the local trash to `remote`.
func (f *trashFs) upload(ctx context.Context, rel, remote string) error {
	if err := f.inner.Mkdir(ctx, remote); err != nil {
		return err
	}
	base := f.localPath(rel)
	return filepath.WalkDir(base, func(p string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		sub, err := filepath.Rel(base, p)
		if err != nil || sub == "." {
			return err
		}
		sub = filepath.ToSlash(sub)
		if d.IsDir() {
			return f.inner.Mkdir(ctx, path.Join(remote, sub))
		}
		o, err := f.local.NewObject(ctx, path.Join(rel, sub))
		if err != nil {
			return err
		}
		_, err = operations.Copy(ctx, f.inner, nil, path.Join(remote, sub), o)
		return err
	})
}

// free: restoring what was at `original` to `remote` won't clash: the file manager restores to where it was, or to a place it chose that is free; the original place must then be free too, as the remote's trash restores there first.
func (f *trashFs) free(ctx context.Context, original, remote string) bool {
	if original == remote {
		return true
	}
	if _, err := f.inner.NewObject(ctx, original); err == nil {
		return false
	}
	_, err := f.inner.List(ctx, original)
	return errors.Is(err, rfs.ErrorDirNotFound)
}

// followRemote keeps in the local trash the files the cache holds in full that left the remote, and drops trash entries whose file came back there. `files` are files the remote reported changed, `dirs` folders; both from the mount's top. Returns whether the trash changed, so the VFS lists it anew.
func (f *trashFs) followRemote(ctx context.Context, files, dirs []string) bool {
	changed := false
	for _, p := range files {
		if f.hidden(p) {
			continue
		}
		if _, ok := f.uploadedCopy(p); ok && gone(f.inner.NewObject(ctx, p)) {
			changed = f.keepGone(ctx, p, false) || changed
		}
	}
	for _, d := range dirs {
		if f.hidden(d) {
			continue
		}
		changed = f.followDir(ctx, d) || changed
	}
	return f.dropReturned(ctx, files, dirs) || changed
}

// followDir checks folder `dir` and what of it the cache holds against the remote.
func (f *trashFs) followDir(ctx context.Context, dir string) bool {
	data, _ := f.cachePaths(dir)
	cachedEntries, err := os.ReadDir(data)
	if err != nil {
		return false
	}
	entries, err := f.inner.List(ctx, dir)
	if errors.Is(err, rfs.ErrorDirNotFound) && dir != "" {
		return f.keepGone(ctx, dir, true)
	}
	if err != nil {
		return false
	}
	listed := map[string]bool{}
	for _, entry := range entries {
		listed[entry.Remote()] = true
	}
	changed := false
	for _, cached := range cachedEntries {
		p := path.Join(dir, encoder.OS.ToStandardName(cached.Name()))
		if listed[p] || (dir == "" && f.hidden(p)) {
			continue
		}
		switch {
		case cached.IsDir():
			changed = f.keepGone(ctx, p, true) || changed
		case cached.Type().IsRegular():
			if _, ok := f.uploadedCopy(p); ok {
				changed = f.keepGone(ctx, p, false) || changed
			}
		}
	}
	return changed
}

// gone: a lookup's error says the file isn't on the remote any more.
func gone(_ rfs.Object, err error) bool {
	return errors.Is(err, rfs.ErrorObjectNotFound) || errors.Is(err, rfs.ErrorDirNotFound)
}

// keepGone puts the cache's full copy of `remote`, which left the remote, into the local trash, with a .trashinfo as the file manager writes one. Returns whether it kept anything.
func (f *trashFs) keepGone(ctx context.Context, remote string, isDir bool) bool {
	if f.knownEntry(remote) {
		return false
	}
	name := f.freeName(path.Base(remote))
	dst := f.localPath("files/" + name)
	if isDir {
		if f.keepCached(remote, dst, true) == 0 {
			_ = os.RemoveAll(dst)
			return false
		}
	} else {
		cached, ok := f.uploadedCopy(remote)
		if !ok || linkOrCopy(cached, dst) != nil {
			return false
		}
	}
	now := time.Now()
	if err := writeTrashInfo(f.localPath("info/"+name+".trashinfo"), remote, now); err != nil {
		rfs.Errorf(f, "couldn't write the trash entry of %s: %v", remote, err)
		_ = os.RemoveAll(dst)
		return false
	}
	f.writeEntry(name, trashEntry{Path: remote, Dir: isDir, Deleted: now, OnRemote: true})
	rfs.Infof(f, "%s left the remote; the copy kept by the cache is in the trash", remote)
	return true
}

// knownEntry: the trash already holds `remote`, trashed in the file manager or kept earlier.
func (f *trashFs) knownEntry(remote string) bool {
	known := false
	f.eachEntry(func(_ string, entry trashEntry) {
		known = known || entry.Path == remote
	})
	return known
}

// eachEntry calls `fn` for each noted trash entry whose file is still in the trash; notes of entries the file manager emptied are deleted.
func (f *trashFs) eachEntry(fn func(name string, entry trashEntry)) {
	notes, err := os.ReadDir(f.entries)
	if err != nil {
		return
	}
	for _, note := range notes {
		name, ok := strings.CutSuffix(note.Name(), ".json")
		if !ok {
			continue
		}
		if _, err := os.Lstat(f.localPath("files/" + name)); err != nil {
			_ = os.Remove(f.entryPath(name))
			continue
		}
		if entry, ok := f.readEntry(name); ok {
			fn(name, entry)
		}
	}
}

// dropReturned drops trash entries that are back on the remote among the reported `files` and `dirs`, e.g. restored in the web app.
func (f *trashFs) dropReturned(ctx context.Context, files, dirs []string) bool {
	reported := func(p string) bool {
		for _, file := range files {
			if file == p {
				return true
			}
		}
		for _, dir := range dirs {
			if dir == p || dir == "" || strings.HasPrefix(p, dir+"/") || path.Dir(p) == dir {
				return true
			}
		}
		return false
	}
	changed := false
	f.eachEntry(func(name string, entry trashEntry) {
		if !reported(entry.Path) {
			return
		}
		var back bool
		if entry.Dir {
			_, err := f.inner.List(ctx, entry.Path)
			back = err == nil
		} else {
			_, err := f.inner.NewObject(ctx, entry.Path)
			back = err == nil
		}
		if back {
			f.removeEntry(name)
			changed = true
		}
	})
	return changed
}

// freeName: `name`, or "name (2).ext" and on, whichever is free in the trash.
func (f *trashFs) freeName(name string) string {
	ext := path.Ext(name)
	stem := strings.TrimSuffix(name, ext)
	if stem == "" {
		stem, ext = name, ""
	}
	candidate := name
	for n := 2; ; n++ {
		_, errFile := os.Lstat(f.localPath("files/" + candidate))
		_, errInfo := os.Lstat(f.localPath("info/" + candidate + ".trashinfo"))
		if errors.Is(errFile, os.ErrNotExist) && errors.Is(errInfo, os.ErrNotExist) {
			return candidate
		}
		candidate = fmt.Sprintf("%s (%d)%s", stem, n, ext)
	}
}

// writeTrashInfo writes a .trashinfo (freedesktop.org trash specification) for `remote`, deleted at `at`. In a trash at the top of a mount the path is relative to the mount's top.
func writeTrashInfo(file, remote string, at time.Time) error {
	segments := strings.Split(remote, "/")
	for i, s := range segments {
		segments[i] = url.PathEscape(s)
	}
	content := fmt.Sprintf("[Trash Info]\nPath=%s\nDeletionDate=%s\n", strings.Join(segments, "/"), at.Format("2006-01-02T15:04:05"))
	if err := os.MkdirAll(filepath.Dir(file), 0o700); err != nil {
		return err
	}
	return os.WriteFile(file, []byte(content), 0o600)
}

// moveLocal moves file `o` of the local trash to `rel`.
func moveLocal(ctx context.Context, local rfs.Fs, o rfs.Object, rel string) (rfs.Object, error) {
	move := local.Features().Move
	if move == nil {
		return nil, rfs.ErrorCantMove
	}
	return move(ctx, o, rel)
}
