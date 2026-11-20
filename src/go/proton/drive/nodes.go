package drive

// Path-free building blocks for the rclone backend (syncative/go/proton/rclonefs): folder listings with each file's extended attributes, renames and moves, restoring from the trash, and reading a file from any offset as a stream.

import (
	"bytes"
	"context"
	"errors"
	"io"
	"strings"
	"time"

	"syncative/go/proton/extras"

	"github.com/ProtonMail/go-proton-api"
	"github.com/ProtonMail/gopenpgp/v2/crypto"
)

// Item is an active link of a folder listing with its decrypted name.
type Item struct {
	Link proton.Link
	Name string
	// xattr is the active revision's encrypted extended attributes from the listing; "" when it had none.
	xattr string
}

// IsDir tells a folder from a file.
func (it *Item) IsDir() bool { return it.Link.Type == proton.LinkTypeFolder }

// ModTime is the time the sync compares: when the active revision was created, see stableModTime.
func (it *Item) ModTime() time.Time { return time.Unix(stableModTime(&it.Link), 0) }

// FileAttrs is what the uploader recorded in a revision's extended attributes. Revisions without them leave Size at -1.
type FileAttrs struct {
	Size       int64
	SHA1       string
	ModTime    time.Time
	BlockSizes []int64
}

// withRefresh runs a request of the extras package, whose HTTP path doesn't renew an expired access token by itself: on a 401 a request through upstream's client renews it, and the request is sent once more.
func (s *Session) withRefresh(ctx context.Context, request func(extras.Auth) error) error {
	err := request(s.protonextAuth())
	var apiErr *proton.APIError
	if errors.As(err, &apiErr) && apiErr.Status == 401 {
		if _, userErr := s.c.GetUser(ctx); userErr != nil {
			return err
		}
		return request(s.protonextAuth())
	}
	return err
}

// Children lists the active children of folder `folderID` ("" for the root) with their names. Folders' links are cached for their keyrings.
func (s *Session) Children(ctx context.Context, folderID string) ([]Item, error) {
	if folderID == "" {
		folderID = s.RootLinkID()
	}
	folder, err := s.getLink(ctx, folderID)
	if err != nil {
		return nil, err
	}
	if folder.State != proton.LinkStateActive {
		return nil, nil
	}
	folderKR, err := s.linkKR(ctx, &folder)
	if err != nil {
		return nil, err
	}
	var children []extras.Child
	if err := s.withRefresh(ctx, func(auth extras.Auth) (err error) {
		children, err = extras.ListChildren(ctx, auth, s.mainShare.ShareID, folderID)
		return err
	}); err != nil {
		return nil, err
	}
	out := make([]Item, 0, len(children))
	for i := range children {
		child := &children[i]
		if child.Link.Type == proton.LinkTypeFolder {
			s.cacheLink(child.Link)
		}
		if child.Link.State != proton.LinkStateActive {
			continue
		}
		name, err := child.Link.GetName(folderKR, s.defaultAddrKR)
		if err != nil {
			return nil, err
		}
		out = append(out, Item{Link: child.Link, Name: name, xattr: child.XAttr})
	}
	return out, nil
}

// ItemByID returns link `linkID` fresh from the API with its name; nil when it isn't active.
func (s *Session) ItemByID(ctx context.Context, linkID string) (*Item, error) {
	link, err := s.refreshLink(ctx, linkID)
	if err != nil {
		return nil, err
	}
	if link.State != proton.LinkStateActive {
		return nil, nil
	}
	item := &Item{Link: link}
	if link.ParentLinkID != "" {
		parentKR, err := s.linkKRByID(ctx, link.ParentLinkID)
		if err != nil {
			return nil, err
		}
		if item.Name, err = link.GetName(parentKR, s.defaultAddrKR); err != nil {
			return nil, err
		}
	}
	return item, nil
}

// Attrs decrypts the extended attributes of file `item`'s active revision, from the listing where it had them and else with one request. Cached by revision, which never changes.
func (s *Session) Attrs(ctx context.Context, item *Item) (*FileAttrs, error) {
	if item.Link.FileProperties == nil {
		return &FileAttrs{Size: -1, ModTime: item.ModTime()}, nil
	}
	revisionID := item.Link.FileProperties.ActiveRevision.ID
	s.cacheMu.Lock()
	cached, ok := s.attrCache[revisionID]
	s.cacheMu.Unlock()
	if ok {
		return cached, nil
	}
	attrs := &FileAttrs{Size: -1, ModTime: item.ModTime()}
	armored := item.xattr
	if armored == "" {
		var rev extras.RevisionWithXAttr
		if err := s.withRefresh(ctx, func(auth extras.Auth) (err error) {
			rev, err = extras.GetRevisionXAttr(ctx, auth, s.mainShare.ShareID, item.Link.LinkID, revisionID)
			return err
		}); err != nil {
			return nil, err
		}
		armored = rev.XAttr
	}
	if armored != "" {
		nodeKR, err := s.fileKR(ctx, &item.Link)
		if err != nil {
			return nil, err
		}
		xa, err := extras.DecryptRevisionXAttr(armored, s.defaultAddrKR, nodeKR)
		if err == nil && xa != nil {
			attrs.Size = xa.Size
			attrs.SHA1 = strings.ToLower(xa.Digests["SHA1"])
			attrs.BlockSizes = xa.BlockSizes
			if t, err := time.Parse("2006-01-02T15:04:05-0700", xa.ModificationTime); err == nil {
				attrs.ModTime = t
			}
		}
	}
	s.cacheMu.Lock()
	if s.attrCache != nil {
		s.attrCache[revisionID] = attrs
	}
	s.cacheMu.Unlock()
	return attrs, nil
}

// fileKR unlocks a file's node keyring through its folder's; not cached, as a large folder would keep a keyring per file.
func (s *Session) fileKR(ctx context.Context, link *proton.Link) (*crypto.KeyRing, error) {
	parentKR, err := s.linkKRByID(ctx, link.ParentLinkID)
	if err != nil {
		return nil, err
	}
	return link.GetKeyRing(parentKR, s.defaultAddrKR)
}

// folderKeys returns folder `folderID`'s node keyring and the key its children's names are hashed with.
func (s *Session) folderKeys(ctx context.Context, folderID string) (*crypto.KeyRing, []byte, error) {
	folder, err := s.getLink(ctx, folderID)
	if err != nil {
		return nil, nil, err
	}
	if folder.Type != proton.LinkTypeFolder {
		return nil, nil, ErrParentNotFolder
	}
	kr, err := s.linkKR(ctx, &folder)
	if err != nil {
		return nil, nil, err
	}
	hashKey, err := folder.GetHashKey(kr)
	if err != nil {
		return nil, nil, err
	}
	return kr, hashKey, nil
}

// Rename gives link `linkID` the name `name` in its folder.
func (s *Session) Rename(ctx context.Context, linkID, name string) error {
	link, err := s.refreshLink(ctx, linkID)
	if err != nil {
		return err
	}
	if link.ParentLinkID == "" {
		return ErrLinkHasNoParent
	}
	parentKR, hashKey, err := s.folderKeys(ctx, link.ParentLinkID)
	if err != nil {
		return err
	}
	req := extras.RenameLinkReq{NameSignatureEmail: s.signatureAddress, OriginalHash: link.Hash}
	if req.Name, err = extras.EncryptName(name, s.defaultAddrKR, parentKR); err != nil {
		return err
	}
	if req.Hash, err = extras.NameHash(name, hashKey); err != nil {
		return err
	}
	err = s.withRefresh(ctx, func(auth extras.Auth) error {
		return extras.RenameLink(ctx, auth, s.volumeID, linkID, req)
	})
	s.forgetLink(linkID)
	return err
}

// Move puts link `linkID` into folder `parentID` under the name `name`.
func (s *Session) Move(ctx context.Context, linkID, parentID, name string) error {
	link, err := s.refreshLink(ctx, linkID)
	if err != nil {
		return err
	}
	if link.ParentLinkID == "" {
		return ErrLinkHasNoParent
	}
	if link.ParentLinkID == parentID {
		return s.Rename(ctx, linkID, name)
	}
	oldParentKR, err := s.linkKRByID(ctx, link.ParentLinkID)
	if err != nil {
		return err
	}
	parentKR, hashKey, err := s.folderKeys(ctx, parentID)
	if err != nil {
		return err
	}
	req := extras.MoveLinkReq{ParentLinkID: parentID, NameSignatureEmail: s.signatureAddress, OriginalHash: link.Hash}
	// The node key itself stays; only the key packet of its passphrase is re-encrypted to the new folder's key.
	if req.NodePassphrase, err = reencryptKeyPacket(oldParentKR, parentKR, s.defaultAddrKR, link.NodePassphrase); err != nil {
		return err
	}
	if req.Name, err = extras.EncryptName(name, s.defaultAddrKR, parentKR); err != nil {
		return err
	}
	if req.Hash, err = extras.NameHash(name, hashKey); err != nil {
		return err
	}
	err = s.withRefresh(ctx, func(auth extras.Auth) error {
		return extras.MoveLink(ctx, auth, s.volumeID, linkID, req)
	})
	s.forgetLink(linkID)
	return err
}

// Restore moves trashed links back where they were.
func (s *Session) Restore(ctx context.Context, linkIDs ...string) error {
	err := s.withRefresh(ctx, func(auth extras.Auth) error {
		return extras.RestoreLinks(ctx, auth, s.volumeID, linkIDs)
	})
	for _, id := range linkIDs {
		s.forgetLink(id)
	}
	return err
}

// TrashedChildren lists the trashed children of folder `folderID` with their names, for restoring them.
func (s *Session) TrashedChildren(ctx context.Context, folderID string) ([]Item, error) {
	folder, err := s.getLink(ctx, folderID)
	if err != nil {
		return nil, err
	}
	folderKR, err := s.linkKR(ctx, &folder)
	if err != nil {
		return nil, err
	}
	var children []extras.Child
	if err := s.withRefresh(ctx, func(auth extras.Auth) (err error) {
		children, err = extras.ListChildren(ctx, auth, s.mainShare.ShareID, folderID)
		return err
	}); err != nil {
		return nil, err
	}
	var out []Item
	for i := range children {
		if children[i].Link.State != proton.LinkStateTrashed {
			continue
		}
		name, err := children[i].Link.GetName(folderKR, s.defaultAddrKR)
		if err != nil {
			return nil, err
		}
		out = append(out, Item{Link: children[i].Link, Name: name})
	}
	return out, nil
}

// Open reads the active revision of file `item` from byte `offset` on, decrypting one block at a time.
func (s *Session) Open(ctx context.Context, item *Item, offset int64) (io.ReadCloser, error) {
	link := item.Link
	if link.Type != proton.LinkTypeFile {
		return nil, ErrLinkIsNotFile
	}
	if link.FileProperties == nil || link.FileProperties.ActiveRevision.ID == "" {
		return nil, ErrNoActiveRevision
	}
	nodeKR, err := s.fileKR(ctx, &link)
	if err != nil {
		return nil, err
	}
	sessionKey, err := link.GetSessionKey(nodeKR)
	if err != nil {
		return nil, err
	}
	revision, err := s.getRevisionAllBlocks(ctx, link.LinkID, link.FileProperties.ActiveRevision.ID)
	if err != nil {
		return nil, err
	}
	r := &blockReader{s: s, ctx: ctx, blocks: revision.Blocks, nodeKR: nodeKR, sessionKey: sessionKey}
	// The recorded plaintext sizes of the blocks let whole blocks before the offset go unfetched.
	if attrs, err := s.Attrs(ctx, item); err == nil && len(attrs.BlockSizes) == len(revision.Blocks) {
		for r.next < len(r.blocks) && offset >= attrs.BlockSizes[r.next] {
			offset -= attrs.BlockSizes[r.next]
			r.next++
		}
	}
	r.skip = offset
	return r, nil
}

// blockReader hands out a revision's plaintext block by block.
type blockReader struct {
	s          *Session
	ctx        context.Context
	blocks     []proton.Block
	next       int
	nodeKR     *crypto.KeyRing
	sessionKey *crypto.SessionKey
	buf        bytes.Buffer
	// skip is how many bytes of plaintext still to drop before the offset.
	skip int64
}

func (r *blockReader) Read(p []byte) (int, error) {
	for r.buf.Len() == 0 {
		if r.next >= len(r.blocks) {
			return 0, io.EOF
		}
		block := &r.blocks[r.next]
		r.next++
		data, err := r.s.fetchBlock(r.ctx, block)
		if err != nil {
			return 0, err
		}
		if err := decryptBlockIntoBuffer(r.sessionKey, r.s.defaultAddrKR, r.nodeKR, block.Hash, block.EncSignature, &r.buf, data); err != nil {
			return 0, err
		}
		if r.skip > 0 {
			n := min(r.skip, int64(r.buf.Len()))
			r.buf.Next(int(n))
			r.skip -= n
		}
	}
	return r.buf.Read(p)
}

func (r *blockReader) Close() error {
	r.buf = bytes.Buffer{}
	return nil
}
