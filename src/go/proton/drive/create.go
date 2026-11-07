package drive

// Folder creation, ported and trimmed from Proton-API-Bridge's
// folder.go::CreateNewFolder.
//
// CreateFolder handles the "already exists" conflict (Proton error
// code 2500, HTTP 422) by looking up the existing folder and returning
// its link ID, making the operation idempotent.

import (
	"context"
	"errors"

	extras "syncative/go/proton"

	"github.com/ProtonMail/go-proton-api"
)

// codeAlreadyExists is Proton's server-side error for "a file or
// folder with that name already exists" (HTTP 422).
const codeAlreadyExists = proton.Code(2500)

// ErrParentNotFolder surfaces when a mkdir / upload target parent
// turns out to be a file. The sync engine enforces this pre-flight
// anyway; this is belt-and-braces.
var ErrParentNotFolder = errors.New("parent link is not a folder")

// CreateFolder creates a new subfolder named `name` under
// `parentLinkID`. Empty parent = session root. Returns the new
// folder's link ID. Idempotent: if a folder with the same name
// already exists, its link ID is returned without hitting the
// create endpoint (avoids noisy 422 / Code=2500 from the server).
func (s *Session) CreateFolder(ctx context.Context, parentLinkID, name string) (string, error) {
	if parentLinkID == "" {
		parentLinkID = s.RootLinkID()
	}

	// Fast path: if a folder with this name already exists under the
	// parent, return its link ID immediately. This avoids the 422
	// round-trip (and resty's WARN/ERROR log noise) plus the wasted
	// crypto work of generating throwaway node keys.
	if existing, err := s.findChildByName(ctx, parentLinkID, name, true); err == nil && existing != "" {
		return existing, nil
	}
	defer s.invalidateListings()

	parentLink, err := s.getLink(ctx, parentLinkID)
	if err != nil {
		return "", err
	}
	if parentLink.Type != proton.LinkTypeFolder {
		return "", ErrParentNotFolder
	}
	parentNodeKR, err := s.linkKR(ctx, &parentLink)
	if err != nil {
		return "", err
	}
	parentHashKey, err := parentLink.GetHashKey(parentNodeKR)
	if err != nil {
		return "", err
	}

	nodeKey, nodePassphraseEnc, nodePassphraseSig, err := generateNodeKeys(parentNodeKR, s.defaultAddrKR)
	if err != nil {
		return "", err
	}

	req := proton.CreateFolderReq{
		ParentLinkID: parentLink.LinkID,

		NodeKey:                 nodeKey,
		NodePassphrase:          nodePassphraseEnc,
		NodePassphraseSignature: nodePassphraseSig,

		SignatureAddress: s.signatureAddress,
	}
	if err := extras.SetCreateFolderName(&req, name, s.defaultAddrKR, parentNodeKR); err != nil {
		return "", err
	}
	if err := extras.SetCreateFolderHash(&req, name, parentHashKey); err != nil {
		return "", err
	}
	// The new folder's own hash key is independent of its parent's;
	// it's used to hash the names of children we create inside it.
	newFolderKR, err := getKeyRing(parentNodeKR, s.defaultAddrKR, nodeKey, nodePassphraseEnc, nodePassphraseSig)
	if err != nil {
		return "", err
	}
	_ = newFolderKR // only needed once we want the hash key back locally
	if err := extras.SetCreateFolderNodeHashKey(&req, newFolderKR); err != nil {
		return "", err
	}

	res, err := s.c.CreateFolder(ctx, s.mainShare.ShareID, req)
	if err != nil {
		// Handle "already exists" — look up the existing folder and
		// return its link ID, making CreateFolder idempotent.
		var apiErr *proton.APIError
		if errors.As(err, &apiErr) && apiErr.Code == codeAlreadyExists {
			existing, lookupErr := s.findChildByName(ctx, parentLink.LinkID, name, true)
			if lookupErr == nil && existing != "" {
				return existing, nil
			}
		}
		return "", err
	}
	return res.ID, nil
}

// findChildByName lists the active children of parentLinkID and returns
// the link ID of the first child matching `name` with the given type
// (isDir=true for folders, false for files). Returns ("", nil) when no
// match is found.
func (s *Session) findChildByName(ctx context.Context, parentLinkID, name string, isDir bool) (string, error) {
	entries, err := s.ListDirectory(ctx, parentLinkID)
	if err != nil {
		return "", err
	}
	for _, e := range entries {
		if e.Name == name && e.IsDir == isDir {
			return e.LinkID, nil
		}
	}
	return "", nil
}

// findChildLinkByName is like findChildByName but returns the full
// proton.Link for the matched child, which callers need when they
// must inspect FileProperties / State / keyrings.
func (s *Session) findChildLinkByName(ctx context.Context, parentLinkID, name string, wantFile bool) (*proton.Link, error) {
	entries, err := s.ListDirectory(ctx, parentLinkID)
	if err != nil {
		return nil, err
	}
	wantType := proton.LinkTypeFolder
	if wantFile {
		wantType = proton.LinkTypeFile
	}
	for _, e := range entries {
		if e.Name == name {
			// Fetch the full link metadata (ListDirectory caches it).
			link, err := s.getLink(ctx, e.LinkID)
			if err != nil {
				continue
			}
			if link.Type == wantType {
				return &link, nil
			}
		}
	}
	return nil, nil
}
