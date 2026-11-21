package rclone

// environment.go: on Android the Go runtime of libsyncative_go.so takes its environment from the process start (bionic hands the original envp to the library's init), before SyncativeApplication pointed HOME, XDG_* and TMPDIR at the app's storage. rclone then puts its cache under /data/local/tmp, which apps can't write. Rust passes the variables over once at start.

import (
	"context"
	"os"
	"path/filepath"

	"github.com/rclone/rclone/fs/config"
	"github.com/rclone/rclone/fs/rc"
)

func init() {
	rc.Add(rc.Call{
		Path:  "syncative/environment",
		Fn:    rcEnvironment,
		Title: "Take over environment variables of the process.",
		Help:  `Parameters: vars, an object of variable names and values. Sets them in Go's environment; with XDG_CACHE_HOME, rclone's cache directory follows.`,
	})
}

func rcEnvironment(ctx context.Context, in rc.Params) (rc.Params, error) {
	var vars map[string]string
	if err := in.GetStruct("vars", &vars); err != nil {
		return nil, err
	}
	for name, value := range vars {
		if err := os.Setenv(name, value); err != nil {
			return nil, err
		}
	}
	if cache, ok := vars["XDG_CACHE_HOME"]; ok && cache != "" {
		if err := config.SetCacheDir(filepath.Join(cache, "rclone")); err != nil {
			return nil, err
		}
	}
	return rc.Params{}, nil
}
