package extras

// Free-function equivalents of the Set* helper methods that used to
// hang off `*proton.CreateFolderReq` / `*proton.CreateFileReq` /
// `*proton.CommitRevisionReq` inside the submodule. Go forbids declaring
// methods on types from another package, so these are package-level
// functions that mutate their request argument in place — the same end
// shape, just spelt differently.

import (
	"encoding/json"

	"github.com/ProtonMail/go-proton-api"
	"github.com/ProtonMail/gopenpgp/v2/crypto"
)

// SetCreateFolderName encrypts `name` to the parent's node keyring,
// signs with the address keyring, and writes the result into req.Name.
func SetCreateFolderName(req *proton.CreateFolderReq, name string, addrKR, nodeKR *crypto.KeyRing) error {
	enc, err := EncryptName(name, addrKR, nodeKR)
	if err != nil {
		return err
	}
	req.Name = enc
	return nil
}

// SetCreateFolderHash writes hex(HMAC-SHA256(name, hashKey)) into
// req.Hash.
func SetCreateFolderHash(req *proton.CreateFolderReq, name string, hashKey []byte) error {
	h, err := NameHash(name, hashKey)
	if err != nil {
		return err
	}
	req.Hash = h
	return nil
}

// SetCreateFolderNodeHashKey generates a fresh hash-key token,
// encrypts it to the parent keyring, and stores the armored result in
// req.NodeHashKey.
func SetCreateFolderNodeHashKey(req *proton.CreateFolderReq, parentNodeKR *crypto.KeyRing) error {
	armored, err := EncryptNodeHashKey(parentNodeKR)
	if err != nil {
		return err
	}
	req.NodeHashKey = armored
	return nil
}

// SetCreateFileName mirrors SetCreateFolderName for file requests.
func SetCreateFileName(req *proton.CreateFileReq, name string, addrKR, nodeKR *crypto.KeyRing) error {
	enc, err := EncryptName(name, addrKR, nodeKR)
	if err != nil {
		return err
	}
	req.Name = enc
	return nil
}

// SetCreateFileHash mirrors SetCreateFolderHash for file requests.
func SetCreateFileHash(req *proton.CreateFileReq, name string, hashKey []byte) error {
	h, err := NameHash(name, hashKey)
	if err != nil {
		return err
	}
	req.Hash = h
	return nil
}

// SetCreateFileContentKey generates a session key, populates the
// ContentKeyPacket + ContentKeyPacketSignature fields, and returns the
// session key so the caller can use it to encrypt blocks.
func SetCreateFileContentKey(req *proton.CreateFileReq, nodeKR *crypto.KeyRing) (*crypto.SessionKey, error) {
	sk, packet, sig, err := GenerateFileSessionKey(nodeKR)
	if err != nil {
		return nil, err
	}
	req.ContentKeyPacket = packet
	req.ContentKeyPacketSignature = sig
	return sk, nil
}

// SetCommitRevisionXAttr encrypts the XAttr common payload + writes it
// into req.XAttr. Mirrors the upstream-shaped CommitRevisionReq we
// declare locally in revision_types.go.
func SetCommitRevisionXAttr(req *CommitRevisionReq, addrKR, nodeKR *crypto.KeyRing, xa *RevisionXAttrCommon) error {
	blob, err := json.Marshal(RevisionXAttr{Common: *xa})
	if err != nil {
		return err
	}
	enc, err := nodeKR.Encrypt(crypto.NewPlainMessage(blob), addrKR)
	if err != nil {
		return err
	}
	armored, err := enc.GetArmored()
	if err != nil {
		return err
	}
	req.XAttr = armored
	return nil
}
