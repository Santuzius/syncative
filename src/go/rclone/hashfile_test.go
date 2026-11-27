package rclone

import (
	"context"
	"os"
	"path/filepath"
	"testing"

	_ "github.com/rclone/rclone/backend/dropbox"
	"github.com/rclone/rclone/fs/rc"
)

// TestHashFileHashesLikeTheProvider: a file holding "J" gets Dropbox's hash (SHA-256 of the SHA-256 of its single block) and OneDrive's QuickXorHash (the vector of rclone's quickxorhash tests), in lowercase hex.
func TestHashFileHashesLikeTheProvider(t *testing.T) {
	path := filepath.Join(t.TempDir(), "j.txt")
	if err := os.WriteFile(path, []byte("J"), 0o600); err != nil {
		t.Fatal(err)
	}
	for kind, want := range map[string]string{
		"dropbox":  "8938fd13801978991ebc1e6c1a731e34d67e3cbec2f171679d6dbc9b1c1d53d6",
		"quickxor": "4a00000000000000000000000100000000000000",
	} {
		out, err := rcHashFile(context.Background(), rc.Params{"path": path, "type": kind})
		if err != nil {
			t.Fatalf("%s: %v", kind, err)
		}
		if out["hash"] != want {
			t.Errorf("%s: %v, want %s", kind, out["hash"], want)
		}
	}
	if _, err := rcHashFile(context.Background(), rc.Params{"path": filepath.Join(path, "missing"), "type": "dropbox"}); err == nil {
		t.Error("missing file: no error")
	}
}
