package extras

// XAttr decryption: counterpart of SetCommitRevisionXAttr. Used by the
// upload path's "skip re-upload of byte-identical content" check —
// reads back an existing revision's plaintext XAttr (size, mod time,
// SHA-1) so we can compare against the local file.

import (
	"encoding/json"

	"github.com/ProtonMail/gopenpgp/v2/crypto"
)

// DecryptRevisionXAttr unwraps the armored PGP blob with the file's
// node keyring, verifies the detached signature against the address
// keyring, and returns the parsed RevisionXAttrCommon. Returns nil
// without error when `armored` is empty (older revisions predating
// Proton's XAttr rollout don't carry one).
func DecryptRevisionXAttr(armored string, addrKR, nodeKR *crypto.KeyRing) (*RevisionXAttrCommon, error) {
	if armored == "" {
		return nil, nil
	}
	msg, err := crypto.NewPGPMessageFromArmored(armored)
	if err != nil {
		return nil, err
	}
	plain, err := nodeKR.Decrypt(msg, addrKR, crypto.GetUnixTime())
	if err != nil {
		return nil, err
	}
	var env RevisionXAttr
	if err := json.Unmarshal(plain.GetBinary(), &env); err != nil {
		return nil, err
	}
	return &env.Common, nil
}
