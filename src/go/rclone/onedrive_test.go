package rclone

import (
	"context"
	"testing"
	"time"

	"github.com/rclone/rclone/fs/config"
	"github.com/rclone/rclone/fs/rc"
)

// A OneDrive remote is created the way the sign-in sends it (src/services/auth/oauth.rs): with token and drive, non-interactive, so the backend's config doesn't go on to ask which drive to use.
func TestOneDriveRemoteIsCreatedWithoutQuestions(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	_, err := rc.Calls.Get("config/create").Fn(ctx, rc.Params{
		"name": "od-test",
		"type": "onedrive",
		"parameters": rc.Params{
			"client_id":            "",
			"client_secret":        "",
			"token":                `{"access_token":"a","token_type":"Bearer","refresh_token":"r","expiry":"2099-01-01T00:00:00Z"}`,
			"config_refresh_token": false,
			"drive_id":             "abc123",
			"drive_type":           "personal",
		},
		"opt": rc.Params{"nonInteractive": true},
	})
	if err != nil {
		t.Fatal(err)
	}
	defer config.DeleteRemote("od-test")
	if got := config.GetValue("od-test", "drive_id"); got != "abc123" {
		t.Errorf("drive_id = %q", got)
	}
	if got := config.GetValue("od-test", "drive_type"); got != "personal" {
		t.Errorf("drive_type = %q", got)
	}
	if got := config.GetValue("od-test", "config_refresh_token"); got != "" {
		t.Errorf("config_refresh_token saved: %q", got)
	}
}
