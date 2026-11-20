package extras

// Link endpoints upstream go-proton-api doesn't expose: a children listing that keeps each file's extended attributes, and rename, move and restore from the trash. Shapes follow Proton's Drive SDK (github.com/ProtonDriveApps/sdk, client/js/src/internal/nodes/apiService.ts).

import (
	"context"
	"encoding/json"
	"fmt"
	"strconv"

	"github.com/ProtonMail/go-proton-api"
)

// childrenPageSize is the largest page the children endpoint returns, as upstream's ListChildren uses it.
const childrenPageSize = 150

// Child is a link of a folder listing with its active revision's encrypted extended attributes ("" when the listing has none), which upstream's Link drops.
type Child struct {
	proton.Link
	XAttr string
}

// ListChildren lists every child of folder `linkID`, all states included, like upstream's ListChildren with showAll.
func ListChildren(ctx context.Context, auth Auth, shareID, linkID string) ([]Child, error) {
	var out []Child
	for page := 0; ; page++ {
		var raw struct {
			Links []json.RawMessage
		}
		path := "/drive/shares/" + shareID + "/folders/" + linkID + "/children?ShowAll=1&PageSize=" + strconv.Itoa(childrenPageSize) + "&Page=" + strconv.Itoa(page)
		if err := do(ctx, auth, "GET", path, nil, &raw); err != nil {
			return nil, err
		}
		if len(raw.Links) == 0 {
			return out, nil
		}
		for _, r := range raw.Links {
			var child Child
			if err := json.Unmarshal(r, &child.Link); err != nil {
				return nil, fmt.Errorf("decode link: %w", err)
			}
			var extra struct {
				FileProperties *struct {
					ActiveRevision struct {
						XAttr *string
					}
				}
			}
			if err := json.Unmarshal(r, &extra); err == nil && extra.FileProperties != nil && extra.FileProperties.ActiveRevision.XAttr != nil {
				child.XAttr = *extra.FileProperties.ActiveRevision.XAttr
			}
			out = append(out, child)
		}
	}
}

// RenameLinkReq renames a link in its folder: the new name encrypted to the folder's node key and its hash with the folder's hash key, and the hash of the current name.
type RenameLinkReq struct {
	Name               string
	NameSignatureEmail string
	Hash               string
	OriginalHash       string
}

// RenameLink renames link `linkID` on volume `volumeID`.
func RenameLink(ctx context.Context, auth Auth, volumeID, linkID string, req RenameLinkReq) error {
	return do(ctx, auth, "PUT", "/drive/v2/volumes/"+volumeID+"/links/"+linkID+"/rename", req, nil)
}

// MoveLinkReq moves a link to another folder: its node passphrase re-encrypted to the new folder's node key, and its name (possibly a new one) encrypted to that key and hashed with that folder's hash key.
type MoveLinkReq struct {
	ParentLinkID       string
	NodePassphrase     string
	Name               string
	NameSignatureEmail string
	Hash               string
	OriginalHash       string
}

// MoveLink moves link `linkID` on volume `volumeID`.
func MoveLink(ctx context.Context, auth Auth, volumeID, linkID string, req MoveLinkReq) error {
	return do(ctx, auth, "PUT", "/drive/v2/volumes/"+volumeID+"/links/"+linkID+"/move", req, nil)
}

// RestoreLinks moves links of volume `volumeID` out of the trash, back where they were. A link the server refused fails the call with that link's error.
func RestoreLinks(ctx context.Context, auth Auth, volumeID string, linkIDs []string) error {
	var res struct {
		Responses []struct {
			LinkID   string
			Response proton.APIError
		}
	}
	body := struct{ LinkIDs []string }{linkIDs}
	if err := do(ctx, auth, "PUT", "/drive/v2/volumes/"+volumeID+"/trash/restore_multiple", body, &res); err != nil {
		return err
	}
	for _, r := range res.Responses {
		if r.Response.Code != proton.SuccessCode {
			return fmt.Errorf("restore %s: %w", r.LinkID, &r.Response)
		}
	}
	return nil
}
