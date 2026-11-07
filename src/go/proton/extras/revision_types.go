package extras

// Wire-shape types for endpoints upstream go-proton-api doesn't expose.
// These mirror the JSON the Proton Drive API consumes/returns.

// CommitRevisionReq is the body for `PUT /drive/shares/{shareID}/files/
// {linkID}/revisions/{revisionID}` when a revision's blocks have all
// been uploaded and the revision is being marked active.
type CommitRevisionReq struct {
	ManifestSignature string
	SignatureAddress  string
	XAttr             string
}

// CreateRevisionRes is the response envelope for `POST /drive/shares/
// {shareID}/files/{linkID}/revisions`.
type CreateRevisionRes struct {
	ID string
}

// RevisionXAttrCommon is the "Common" payload inside a revision's
// XAttr blob. ModificationTime is ISO 8601, BlockSizes records each
// block's plaintext byte count, Digests carries SHA-1 of the original
// file content (used by the byte-identical short-circuit).
type RevisionXAttrCommon struct {
	ModificationTime string
	Size             int64
	BlockSizes       []int64
	Digests          map[string]string
}

// RevisionXAttr is the JSON envelope encrypted into
// CommitRevisionReq.XAttr.
type RevisionXAttr struct {
	Common RevisionXAttrCommon
}
