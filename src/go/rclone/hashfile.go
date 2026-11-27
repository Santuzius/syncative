package rclone

// hashfile.go hashes a local file the way a provider hashes its copy, so the sync can tell whether both hold the same content: Dropbox and OneDrive keep no SHA-1 but hashes of their own, which rclone's backends implement.

import (
	"context"
	"io"
	"os"

	"github.com/rclone/rclone/fs/hash"
	"github.com/rclone/rclone/fs/rc"
)

func init() {
	rc.Add(rc.Call{
		Path:  "syncative/hash-file",
		Fn:    rcHashFile,
		Title: "The hash of a local file, as a provider reports it.",
		Help:  `Parameters: path, the local file; type, an rclone hash name such as "dropbox" or "quickxor". Returns hash, lowercase hex.`,
	})
}

func rcHashFile(ctx context.Context, in rc.Params) (rc.Params, error) {
	path, err := in.GetString("path")
	if err != nil {
		return nil, err
	}
	name, err := in.GetString("type")
	if err != nil {
		return nil, err
	}
	var kind hash.Type
	if err := kind.Set(name); err != nil {
		return nil, err
	}
	sum, err := fileHash(path, kind)
	if err != nil {
		return nil, err
	}
	return rc.Params{"hash": sum}, nil
}

// fileHash is the hash of kind `kind` of the file at `path`, in lowercase hex.
func fileHash(path string, kind hash.Type) (string, error) {
	file, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer file.Close()
	hasher, err := hash.NewMultiHasherTypes(hash.NewHashSet(kind))
	if err != nil {
		return "", err
	}
	if _, err := io.Copy(hasher, file); err != nil {
		return "", err
	}
	return hasher.SumString(kind, false)
}
