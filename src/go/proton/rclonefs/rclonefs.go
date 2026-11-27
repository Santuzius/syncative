// Package rclonefs makes Syncative's Proton Drive client (syncative/go/proton/drive) an rclone backend, type "native-proton", so the sync and rclone's VFS reach Proton through the same calls as Google Drive or Dropbox.
//
// The backend holds no credentials: the session is signed in and resumed by Syncative (src/services/auth/proton.rs), which binds it to the remote's name with drive.BindRemote. Every call looks the session up anew, so a new sign-in takes effect without recreating the backend.
//
// Paths become link IDs through rclone's directory cache for folders, and by listing the folder for files. The volume's event log answers Syncative's change query (src/go/rclone/changes/changes.go) by link ID, so the sync places each change by the IDs it recorded.
package rclonefs

import (
	"context"
	"errors"
	"fmt"
	"io"
	"path"
	"strings"
	"sync"
	"time"

	"syncative/go/proton/drive"
	"syncative/go/rclone/changes"

	"github.com/rclone/rclone/backend/overview"
	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/fs/config"
	"github.com/rclone/rclone/fs/config/configmap"
	"github.com/rclone/rclone/fs/config/configstruct"
	"github.com/rclone/rclone/fs/hash"
	"github.com/rclone/rclone/lib/dircache"
	"github.com/rclone/rclone/lib/encoder"
	"github.com/rclone/rclone/lib/readers"
)

// TypeName is the backend's type in rclone's config, the same string Syncative stores as the remote's backend.
const TypeName = "native-proton"

func init() {
	fs.Register(&fs.RegInfo{
		Name:        TypeName,
		Description: "Proton Drive, signed in by Syncative",
		NewFs:       NewFs,
		// rclone's documentation data covers only its own backends.
		Overview: new(overview.BackendConfig),
		Options: []fs.Option{{
			Name:     config.ConfigEncoding,
			Help:     config.ConfigEncodingHelp,
			Advanced: true,
			// Proton takes any name; only what rclone can't put in a path is encoded.
			Default: encoder.Base,
		}},
	})
}

// Options are the backend's config options.
type Options struct {
	Enc encoder.MultiEncoder `config:"encoding"`
}

// Fs is a Proton Drive remote.
type Fs struct {
	name     string
	root     string
	opt      Options
	features *fs.Features
	dirCache *dircache.DirCache
	recent   *listings
	files    *filePaths
}

// filePaths remembers where listed files lie once a mount follows the event log (ChangesStartToken): the log names a file by its link ID and its new parent only, and the VFS has to forget the folder it left too. The directory cache does the same for folders.
type filePaths struct {
	mu   sync.Mutex
	on   bool
	byID map[string]string
}

// put records file `id` at `remote`, once a mount follows the log.
func (p *filePaths) put(id, remote string) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.on {
		p.byID[id] = remote
	}
}

// take returns where file `id` was listed and forgets it.
func (p *filePaths) take(id string) (string, bool) {
	p.mu.Lock()
	defer p.mu.Unlock()
	remote, ok := p.byID[id]
	delete(p.byID, id)
	return remote, ok
}

// listings keeps the last folder listings for the lookups of single items that follow them: rclone finds each file it copies by its path, and Proton can only list the whole folder for it. Bounded in time, so other clients' changes show soon, and in number, as a listing holds every item's keys.
type listings struct {
	mu      sync.Mutex
	byDir   map[string]listing
	ordered []string
}

type listing struct {
	items []drive.Item
	at    time.Time
}

const (
	listingTTL  = 30 * time.Second
	maxListings = 4
)

func (l *listings) get(dirID string) ([]drive.Item, bool) {
	l.mu.Lock()
	defer l.mu.Unlock()
	found, ok := l.byDir[dirID]
	if !ok || time.Since(found.at) > listingTTL {
		return nil, false
	}
	return found.items, true
}

func (l *listings) put(dirID string, items []drive.Item) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if _, ok := l.byDir[dirID]; !ok {
		l.ordered = append(l.ordered, dirID)
	}
	l.byDir[dirID] = listing{items: items, at: time.Now()}
	if len(l.ordered) > maxListings {
		delete(l.byDir, l.ordered[0])
		l.ordered = l.ordered[1:]
	}
}

// update replaces or adds `item` in folder `dirID`'s listing, or removes link `gone` from it, when the listing is kept.
func (l *listings) update(dirID string, item *drive.Item, gone string) {
	l.mu.Lock()
	defer l.mu.Unlock()
	found, ok := l.byDir[dirID]
	if !ok {
		return
	}
	items := make([]drive.Item, 0, len(found.items)+1)
	for _, it := range found.items {
		if it.Link.LinkID != gone && (item == nil || it.Link.LinkID != item.Link.LinkID) {
			items = append(items, it)
		}
	}
	if item != nil {
		items = append(items, *item)
	}
	found.items = items
	l.byDir[dirID] = found
}

func (l *listings) clear() {
	l.mu.Lock()
	defer l.mu.Unlock()
	clear(l.byDir)
	l.ordered = nil
}

// children lists folder `dirID`, from a recent listing unless `fresh`.
func (f *Fs) children(ctx context.Context, sess *drive.Session, dirID string, fresh bool) ([]drive.Item, error) {
	if !fresh {
		if items, ok := f.recent.get(dirID); ok {
			return items, nil
		}
	}
	items, err := sess.Children(ctx, dirID)
	if err != nil {
		return nil, err
	}
	f.recent.put(dirID, items)
	return items, nil
}

// child returns the item `name` of the given kind in folder `dirID`; nil when there is none (see drive.ChildNamed).
func (f *Fs) child(ctx context.Context, sess *drive.Session, dirID, name string, isDir bool) (*drive.Item, error) {
	items, err := f.children(ctx, sess, dirID, false)
	if err != nil {
		return nil, err
	}
	found := drive.ChildNamed(items, name, isDir)
	if found == nil {
		return nil, nil
	}
	item := *found
	return &item, nil
}

// Object is a file on the remote.
type Object struct {
	fs     *Fs
	remote string
	item   drive.Item

	// attrsMu guards attrs, the revision's extended attributes once fetched.
	attrsMu sync.Mutex
	attrs   *drive.FileAttrs
}

// NewFs makes the backend for remote `name` with the path `root`.
func NewFs(ctx context.Context, name, root string, m configmap.Mapper) (fs.Fs, error) {
	opt := new(Options)
	if err := configstruct.Set(m, opt); err != nil {
		return nil, err
	}
	sess, err := drive.ForRemote(name)
	if err != nil {
		return nil, err
	}
	root = strings.Trim(path.Clean("/"+root), "/")
	f := &Fs{name: name, root: root, opt: *opt, recent: &listings{byDir: map[string]listing{}}, files: &filePaths{byID: map[string]string{}}}
	f.features = (&fs.Features{
		CanHaveEmptyDirectories: true,
		ReadMimeType:            true,
		ReadMetadata:            true,
	}).Fill(ctx, f)
	rootID := sess.RootLinkID()
	f.dirCache = dircache.New(root, rootID, f)
	if err := f.dirCache.FindRoot(ctx, false); err != nil {
		// The root may be a file: then the backend is its folder's, as rclone expects.
		newRoot, leaf := dircache.SplitPath(root)
		tempF := *f
		tempF.dirCache = dircache.New(newRoot, rootID, &tempF)
		tempF.root = newRoot
		if err := tempF.dirCache.FindRoot(ctx, false); err != nil {
			return f, nil
		}
		if _, err := tempF.NewObject(ctx, leaf); err != nil {
			if errors.Is(err, fs.ErrorObjectNotFound) {
				return f, nil
			}
			return nil, err
		}
		f.features.Fill(ctx, &tempF)
		f.dirCache = tempF.dirCache
		f.root = tempF.root
		return f, fs.ErrorIsFile
	}
	return f, nil
}

// session is the remote's session as it is now.
func (f *Fs) session() (*drive.Session, error) {
	return drive.ForRemote(f.name)
}

// Name of the remote (as passed into NewFs).
func (f *Fs) Name() string { return f.name }

// Root of the remote (as passed into NewFs).
func (f *Fs) Root() string { return f.root }

// String converts this Fs to a string.
func (f *Fs) String() string { return fmt.Sprintf("Proton Drive root '%s'", f.root) }

// Features returns the optional features of this Fs.
func (f *Fs) Features() *fs.Features { return f.features }

// Precision of the modification times: whole seconds. The times are those of the revisions, see drive.Item.ModTime.
func (f *Fs) Precision() time.Duration { return time.Second }

// Hashes are the SHA-1 the uploader recorded.
func (f *Fs) Hashes() hash.Set { return hash.Set(hash.SHA1) }

// DirCacheFlush forgets the folders' IDs.
func (f *Fs) DirCacheFlush() {
	f.dirCache.ResetRoot()
	f.recent.clear()
}

func (f *Fs) newObject(remote string, item drive.Item) *Object {
	return &Object{fs: f, remote: remote, item: item}
}

// List the items in folder `dir`.
func (f *Fs) List(ctx context.Context, dir string) (fs.DirEntries, error) {
	sess, err := f.session()
	if err != nil {
		return nil, err
	}
	dirID, err := f.dirCache.FindDir(ctx, dir, false)
	if err != nil {
		return nil, err
	}
	items, err := f.children(ctx, sess, dirID, true)
	if err != nil {
		return nil, err
	}
	entries := make(fs.DirEntries, 0, len(items))
	for _, item := range items {
		remote := path.Join(dir, f.opt.Enc.ToStandardName(item.Name))
		if item.IsDir() {
			f.dirCache.Put(remote, item.Link.LinkID)
			entries = append(entries, fs.NewDir(remote, item.ModTime()).SetID(item.Link.LinkID))
			continue
		}
		f.files.put(item.Link.LinkID, remote)
		entries = append(entries, f.newObject(remote, item))
	}
	return entries, nil
}

// FindLeaf finds folder `leaf` in folder `pathID`, for the directory cache.
func (f *Fs) FindLeaf(ctx context.Context, pathID, leaf string) (string, bool, error) {
	sess, err := f.session()
	if err != nil {
		return "", false, err
	}
	item, err := f.child(ctx, sess, pathID, f.opt.Enc.FromStandardName(leaf), true)
	if err != nil || item == nil {
		return "", false, err
	}
	return item.Link.LinkID, true, nil
}

// CreateDir makes folder `leaf` in folder `pathID`, for the directory cache.
func (f *Fs) CreateDir(ctx context.Context, pathID, leaf string) (string, error) {
	sess, err := f.session()
	if err != nil {
		return "", err
	}
	id, err := sess.CreateFolder(ctx, pathID, f.opt.Enc.FromStandardName(leaf))
	f.recent.clear()
	return id, err
}

// NewObject finds the file at `remote`, fresh from its folder's listing.
func (f *Fs) NewObject(ctx context.Context, remote string) (fs.Object, error) {
	sess, err := f.session()
	if err != nil {
		return nil, err
	}
	leaf, dirID, err := f.dirCache.FindPath(ctx, remote, false)
	if err != nil {
		if errors.Is(err, fs.ErrorDirNotFound) {
			return nil, fs.ErrorObjectNotFound
		}
		return nil, err
	}
	item, err := f.child(ctx, sess, dirID, f.opt.Enc.FromStandardName(leaf), false)
	if err != nil {
		return nil, err
	}
	if item == nil {
		return nil, fs.ErrorObjectNotFound
	}
	return f.newObject(remote, *item), nil
}

// Put uploads a file, as a new revision when it exists.
func (f *Fs) Put(ctx context.Context, in io.Reader, src fs.ObjectInfo, options ...fs.OpenOption) (fs.Object, error) {
	existing, err := f.NewObject(ctx, src.Remote())
	switch {
	case err == nil:
		return existing, existing.Update(ctx, in, src, options...)
	case errors.Is(err, fs.ErrorObjectNotFound):
		o := &Object{fs: f, remote: src.Remote()}
		return o, o.Update(ctx, in, src, options...)
	default:
		return nil, err
	}
}

// Mkdir makes folder `dir` and the folders above it.
func (f *Fs) Mkdir(ctx context.Context, dir string) error {
	_, err := f.dirCache.FindDir(ctx, dir, true)
	return err
}

// Rmdir trashes folder `dir` if it is empty.
func (f *Fs) Rmdir(ctx context.Context, dir string) error {
	return f.trashDir(ctx, dir, true)
}

// Purge trashes folder `dir` with everything in it; Proton keeps it in the trash.
func (f *Fs) Purge(ctx context.Context, dir string) error {
	return f.trashDir(ctx, dir, false)
}

func (f *Fs) trashDir(ctx context.Context, dir string, onlyEmpty bool) error {
	sess, err := f.session()
	if err != nil {
		return err
	}
	if dir == "" && f.root == "" {
		return errors.New("can't remove the drive's root")
	}
	dirID, err := f.dirCache.FindDir(ctx, dir, false)
	if err != nil {
		return err
	}
	if onlyEmpty {
		children, err := f.children(ctx, sess, dirID, true)
		if err != nil {
			return err
		}
		if len(children) > 0 {
			return fs.ErrorDirectoryNotEmpty
		}
	}
	err = sess.TrashLink(ctx, dirID)
	f.recent.clear()
	if err != nil {
		return err
	}
	f.dirCache.FlushDir(dir)
	return nil
}

// Move moves (and renames) file `src` to `remote`.
func (f *Fs) Move(ctx context.Context, src fs.Object, remote string) (fs.Object, error) {
	srcObj, ok := src.(*Object)
	if !ok || srcObj.fs.name != f.name {
		return nil, fs.ErrorCantMove
	}
	sess, err := f.session()
	if err != nil {
		return nil, err
	}
	leaf, dirID, err := f.dirCache.FindPath(ctx, remote, true)
	if err != nil {
		return nil, err
	}
	id := srcObj.item.Link.LinkID
	err = sess.Move(ctx, id, dirID, f.opt.Enc.FromStandardName(leaf))
	srcObj.fs.recent.update(srcObj.item.Link.ParentLinkID, nil, id)
	if err != nil {
		return nil, err
	}
	item, err := sess.ItemByID(ctx, id)
	if err != nil {
		return nil, err
	}
	if item == nil {
		return nil, fs.ErrorObjectNotFound
	}
	f.recent.update(dirID, item, "")
	return f.newObject(remote, *item), nil
}

// DirMove moves (and renames) folder `srcRemote` of `src` to `dstRemote`.
func (f *Fs) DirMove(ctx context.Context, src fs.Fs, srcRemote, dstRemote string) error {
	srcFs, ok := src.(*Fs)
	if !ok || srcFs.name != f.name {
		return fs.ErrorCantDirMove
	}
	sess, err := f.session()
	if err != nil {
		return err
	}
	srcID, _, _, dstDirID, dstLeaf, err := f.dirCache.DirMove(ctx, srcFs.dirCache, srcFs.root, srcRemote, f.root, dstRemote)
	if err != nil {
		return err
	}
	err = sess.Move(ctx, srcID, dstDirID, f.opt.Enc.FromStandardName(dstLeaf))
	srcFs.recent.clear()
	f.recent.clear()
	if err != nil {
		return err
	}
	srcFs.dirCache.FlushDir(srcRemote)
	return nil
}

// About reports the account's storage: one plan pools Mail, Drive and the other services.
func (f *Fs) About(ctx context.Context) (*fs.Usage, error) {
	sess, err := f.session()
	if err != nil {
		return nil, err
	}
	quota, err := sess.Quota(ctx)
	if err != nil {
		return nil, err
	}
	used, total := int64(quota.Used), int64(quota.Max)
	usage := &fs.Usage{Used: &used}
	if total > 0 {
		free := max(total-used, 0)
		usage.Total, usage.Free = &total, &free
	}
	return usage, nil
}

// Fs returns the backend the file belongs to.
func (o *Object) Fs() fs.Info { return o.fs }

// Remote returns the file's path.
func (o *Object) Remote() string { return o.remote }

// String returns the file's path.
func (o *Object) String() string {
	if o == nil {
		return "<nil>"
	}
	return o.remote
}

// ID returns the file's link ID.
func (o *Object) ID() string { return o.item.Link.LinkID }

// MimeType returns the type Proton recorded.
func (o *Object) MimeType(ctx context.Context) string { return o.item.Link.MIMEType }

// Storable says the file can be copied.
func (o *Object) Storable() bool { return true }

// ModTime is when the active revision was created, the time the sync compares.
func (o *Object) ModTime(ctx context.Context) time.Time { return o.item.ModTime() }

// SetModTime can't be done without a new revision.
func (o *Object) SetModTime(ctx context.Context, t time.Time) error { return fs.ErrorCantSetModTime }

// Metadata gives, as "mtime", the time the file was last modified before its upload, from the revision's extended attributes (the revision's time where it has none): downloads take that time, while ModTime stays the revision's, the one the sync compares. An error when the attributes couldn't be fetched, so that a download fails instead of taking the upload's time.
func (o *Object) Metadata(ctx context.Context) (fs.Metadata, error) {
	attrs, err := o.fileAttrs(ctx)
	if err != nil {
		return nil, err
	}
	return fs.Metadata{"mtime": attrs.ModTime.UTC().Format(time.RFC3339Nano)}, nil
}

// ModifiedTime is the time the file was last modified before its upload, which a network drive shows (src/go/rclone/modtime/modtime.go); the revision's time when the attributes couldn't be fetched.
func (o *Object) ModifiedTime(ctx context.Context) time.Time {
	if attrs, err := o.fileAttrs(ctx); err == nil {
		return attrs.ModTime
	}
	return o.ModTime(ctx)
}

// fileAttrs decrypts the revision's extended attributes once; an error when they couldn't be fetched, and the next call asks again: the errors are the network's or the session's (attributes that don't decrypt give defaults), and the VFS keeps an object for long.
func (o *Object) fileAttrs(ctx context.Context) (*drive.FileAttrs, error) {
	o.attrsMu.Lock()
	defer o.attrsMu.Unlock()
	if o.attrs != nil {
		return o.attrs, nil
	}
	sess, err := o.fs.session()
	if err != nil {
		return nil, err
	}
	attrs, err := sess.Attrs(ctx, &o.item)
	if err != nil {
		fs.Debugf(o, "extended attributes: %v", err)
		return nil, fmt.Errorf("extended attributes: %w", err)
	}
	o.attrs = attrs
	return attrs, nil
}

// Size is the plaintext size the uploader recorded; -1 when the revision has none, as the encrypted size Proton keeps is larger.
func (o *Object) Size() int64 {
	if attrs, err := o.fileAttrs(context.Background()); err == nil {
		return attrs.Size
	}
	return -1
}

// Hash returns the SHA-1 the uploader recorded; "" when there is none.
func (o *Object) Hash(ctx context.Context, t hash.Type) (string, error) {
	if t != hash.SHA1 {
		return "", hash.ErrUnsupported
	}
	if attrs, err := o.fileAttrs(ctx); err == nil {
		return attrs.SHA1, nil
	}
	return "", nil
}

// Open reads the file, from the offset and to the end a seek or range option gives.
func (o *Object) Open(ctx context.Context, options ...fs.OpenOption) (io.ReadCloser, error) {
	sess, err := o.fs.session()
	if err != nil {
		return nil, err
	}
	var offset, limit int64 = 0, -1
	fs.FixRangeOption(options, o.Size())
	for _, option := range options {
		switch x := option.(type) {
		case *fs.SeekOption:
			offset = x.Offset
		case *fs.RangeOption:
			offset, limit = x.Decode(o.Size())
		default:
			if option.Mandatory() {
				fs.Logf(o, "Unsupported mandatory option: %v", option)
			}
		}
	}
	rc, err := sess.Open(ctx, &o.item, offset)
	if err != nil {
		return nil, err
	}
	if limit >= 0 {
		return readers.NewLimitedReadCloser(rc, limit), nil
	}
	return rc, nil
}

// Update uploads `in` as the file's new content: a new revision, or the file itself when it is new.
func (o *Object) Update(ctx context.Context, in io.Reader, src fs.ObjectInfo, options ...fs.OpenOption) error {
	sess, err := o.fs.session()
	if err != nil {
		return err
	}
	leaf, dirID, err := o.fs.dirCache.FindPath(ctx, o.remote, true)
	if err != nil {
		return err
	}
	// The SHA-1 of a local file lets an identical upload be skipped; it costs a read of the file, which an upload does anyway.
	var localSHA1 func() (string, error)
	if src.Fs() != nil && src.Fs().Hashes().Contains(hash.SHA1) {
		localSHA1 = func() (string, error) {
			sum, err := src.Hash(ctx, hash.SHA1)
			if err == nil && sum == "" {
				err = errors.New("no SHA-1")
			}
			return sum, err
		}
	}
	id, err := sess.Upload(ctx, dirID, o.fs.opt.Enc.FromStandardName(leaf), in, src.ModTime(ctx), localSHA1)
	if err != nil {
		return err
	}
	item, err := sess.ItemByID(ctx, id)
	if err != nil {
		return err
	}
	if item == nil {
		return fs.ErrorObjectNotFound
	}
	o.fs.recent.update(dirID, item, "")
	o.attrsMu.Lock()
	o.item = *item
	o.attrs = nil
	o.attrsMu.Unlock()
	return nil
}

// Remove moves the file to Proton's trash.
func (o *Object) Remove(ctx context.Context) error {
	sess, err := o.fs.session()
	if err != nil {
		return err
	}
	err = sess.TrashLink(ctx, o.item.Link.LinkID)
	o.fs.recent.update(o.item.Link.ParentLinkID, nil, o.item.Link.LinkID)
	return err
}

// Untrash takes link `id` out of Proton's trash, back where it was, for the network drive's trash (src/go/rclone/trash/trashbin.go).
func (f *Fs) Untrash(ctx context.Context, id string) error {
	sess, err := f.session()
	if err != nil {
		return err
	}
	err = sess.Restore(ctx, id)
	f.recent.clear()
	f.dirCache.Flush()
	return err
}

// ChangesRoot returns the link ID of folder `dir`, for the change query.
func (f *Fs) ChangesRoot(ctx context.Context, dir string) (string, error) {
	return f.dirCache.FindDir(ctx, dir, false)
}

// LatestItemEvent is the volume's event log as it stands now.
func (f *Fs) LatestItemEvent(ctx context.Context) (string, error) {
	sess, err := f.session()
	if err != nil {
		return "", err
	}
	return sess.LatestEventID(ctx)
}

// ItemChangesSince returns the links that changed after `event`.
func (f *Fs) ItemChangesSince(ctx context.Context, event string) (changes.ItemChanges, error) {
	found, err := f.changesSince(ctx, event, nil)
	if err != nil {
		return changes.ItemChanges{}, err
	}
	out := changes.ItemChanges{Next: found.Cursor, Relist: found.Relist}
	for _, c := range found.Changes {
		out.Items = append(out.Items, changes.ItemChange{
			ID:          c.LinkID,
			ParentID:    c.ParentLinkID,
			Name:        f.opt.Enc.ToStandardName(c.Name),
			IsDir:       c.IsDir,
			ModTimeUnix: c.ModTimeUnix,
			Gone:        c.Gone,
		})
	}
	return out, nil
}

// changesSince reads the event log after `event` and forgets what the changes leave stale: the kept listings, and the directory cache's entries of folders that moved or went, where their old paths would still lead. notify, when given, gets each path below the root that changed: where an item was and where it is now.
func (f *Fs) changesSince(ctx context.Context, event string, notify func(string, fs.EntryType)) (*drive.Changes, error) {
	sess, err := f.session()
	if err != nil {
		return nil, err
	}
	found, err := sess.ChangesSince(ctx, event)
	if err != nil {
		return nil, err
	}
	if len(found.Changes) > 0 || found.Relist {
		// Another client changed the drive: what the kept listings show may be gone.
		f.recent.clear()
	}
	type changed struct {
		remote string
		kind   fs.EntryType
	}
	var paths []changed
	var flush []string
	for _, c := range found.Changes {
		kind := fs.EntryObject
		var old string
		var known bool
		if c.IsDir {
			kind = fs.EntryDirectory
			old, known = f.dirCache.GetInv(c.LinkID)
			if known {
				flush = append(flush, old)
			}
		} else {
			old, known = f.files.take(c.LinkID)
		}
		if known {
			paths = append(paths, changed{old, kind})
		}
		if parent, ok := f.dirCache.GetInv(c.ParentLinkID); ok && !c.Gone && c.Name != "" {
			remote := path.Join(parent, f.opt.Enc.ToStandardName(c.Name))
			if !known || remote != old {
				paths = append(paths, changed{remote, kind})
			}
			if !c.IsDir {
				f.files.put(c.LinkID, remote)
			}
		}
	}
	for _, dir := range flush {
		f.dirCache.FlushDir(dir)
	}
	if notify != nil {
		for _, p := range paths {
			notify(p.remote, p.kind)
		}
	}
	return found, nil
}

// ChangesStartToken is where the event log stands now, for ChangesSince. From now on the backend remembers where it lists files, so ChangesSince can report the folder a moved file left.
func (f *Fs) ChangesStartToken(ctx context.Context) (string, error) {
	f.files.mu.Lock()
	f.files.on = true
	f.files.mu.Unlock()
	return f.LatestItemEvent(ctx)
}

// ChangesSince calls notify with the paths below the root that changed after event `token` and returns the event after them; "" when the log can't be followed from `token`. A network drive's pass asks it, for the mount to forget those paths (src/go/rclone/netdrive/poll.go).
func (f *Fs) ChangesSince(ctx context.Context, notify func(string, fs.EntryType), token string) (string, error) {
	found, err := f.changesSince(ctx, token, notify)
	if err != nil {
		return "", err
	}
	if found.Relist {
		return "", nil
	}
	return found.Cursor, nil
}

// Check the interfaces are satisfied.
var (
	_ fs.Fs              = (*Fs)(nil)
	_ fs.Purger          = (*Fs)(nil)
	_ fs.Mover           = (*Fs)(nil)
	_ fs.DirMover        = (*Fs)(nil)
	_ fs.Abouter         = (*Fs)(nil)
	_ fs.DirCacheFlusher = (*Fs)(nil)
	_ fs.Object          = (*Object)(nil)
	_ fs.MimeTyper       = (*Object)(nil)
	_ fs.IDer            = (*Object)(nil)
	_ fs.Metadataer      = (*Object)(nil)
)
