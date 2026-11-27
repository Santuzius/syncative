package main

/*
#include <stdlib.h>
*/
import "C"

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"sync"
	"time"

	"github.com/rclone/rclone/backend/onedrive/api"
	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/fs/config"
	"github.com/rclone/rclone/fs/config/configmap"
	"github.com/rclone/rclone/fs/fshttp"
	"github.com/rclone/rclone/lib/oauthutil"
	"github.com/rclone/rclone/lib/rest"
)

// What `rclone authorize` does, inside Syncative: the rclone binary is not there on Android, and a separate process is not needed elsewhere either. Instead of opening a browser itself rclone hands the authorization link over (see SyncativeAuthorizeURL), so Syncative can open it in the platform's way.

var (
	authorizeURLMutex sync.Mutex
	authorizeURL      string
)

func init() {
	oauthutil.OpenURL = func(url string) error {
		authorizeURLMutex.Lock()
		authorizeURL = url
		authorizeURLMutex.Unlock()
		return nil
	}
}

// SyncativeAuthorize runs rclone's OAuth flow for the backend `typ` ("drive", "dropbox", "onedrive"), with the given client ID and secret unless empty, and returns `{"token": "…", "params": {…}}` or `{"error": "…"}`. params holds what else the remote's config needs, see driveParams. Blocks until the browser returns to rclone's local web server, or SyncativeAuthorizeCancel. Free the result with RcloneFreeString.
//
//export SyncativeAuthorize
func SyncativeAuthorize(typ, clientID, clientSecret *C.char) (out *C.char) {
	defer recoverJSON(&out)
	// Cleared on the way out as well, so the next run never sees this run's link.
	clearURL := func() {
		authorizeURLMutex.Lock()
		authorizeURL = ""
		authorizeURLMutex.Unlock()
	}
	clearURL()
	defer clearURL()

	reply := map[string]any{}
	kind := C.GoString(typ)
	if token, err := authorize(kind, C.GoString(clientID), C.GoString(clientSecret)); err != nil {
		reply["error"] = err.Error()
	} else if params, err := driveParams(kind, token); err != nil {
		reply["error"] = err.Error()
	} else {
		reply["token"] = token
		reply["params"] = params
	}
	body, _ := json.Marshal(reply)
	return C.CString(string(body))
}

// SyncativeAuthorizeURL returns the authorization link of the running SyncativeAuthorize, or NULL before rclone has made it. Free it with RcloneFreeString.
//
//export SyncativeAuthorizeURL
func SyncativeAuthorizeURL() (out *C.char) {
	defer recoverJSON(&out)
	authorizeURLMutex.Lock()
	defer authorizeURLMutex.Unlock()
	if authorizeURL == "" {
		return nil
	}
	return C.CString(authorizeURL)
}

// SyncativeAuthorizeCancel makes a running SyncativeAuthorize return with an error. rclone waits for its web server without a timeout, so this calls the server the way the browser would, only without a code.
//
//export SyncativeAuthorizeCancel
func SyncativeAuthorizeCancel() {
	defer recoverLogged()
	client := http.Client{Timeout: 2 * time.Second}
	if resp, err := client.Get(oauthutil.RedirectURL); err == nil {
		resp.Body.Close()
	}
}

// authorize mirrors config.Authorize with a client ID and secret, but returns the token instead of printing it.
func authorize(typ, clientID, clientSecret string) (string, error) {
	ctx, ci := fs.AddConfig(context.Background())
	ci.AutoConfirm = true
	ctx = fs.ConfigOAuthOnly(ctx)
	ri, err := fs.Find(typ)
	if err != nil {
		return "", err
	}

	in := configmap.Simple{config.ConfigAuthorize: "true"}
	if clientID != "" && clientSecret != "" {
		in[config.ConfigClientID] = clientID
		in[config.ConfigClientSecret] = clientSecret
	}
	const name = "**temp-fs**"
	m := fs.ConfigMap(ri.Prefix, ri.Options, name, in)
	out := configmap.Simple{}
	m.ClearSetters()
	m.AddSetter(out)
	m.AddGetter(out, configmap.PriorityNormal)
	if err := config.PostConfig(ctx, name, m, ri); err != nil {
		return "", err
	}
	return out["token"], nil
}

// driveParams is what the config of a remote of backend `typ` needs besides the token. A OneDrive remote names the drive it uses: the account's own, which `rclone config` offers first as well.
func driveParams(typ, token string) (map[string]string, error) {
	if typ != "onedrive" {
		return map[string]string{}, nil
	}
	var tok struct {
		AccessToken string `json:"access_token"`
	}
	if err := json.Unmarshal([]byte(token), &tok); err != nil {
		return nil, fmt.Errorf("reading the token: %w", err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), time.Minute)
	defer cancel()
	srv := rest.NewClient(fshttp.NewClient(ctx)).SetRoot("https://graph.microsoft.com/v1.0").SetHeader("Authorization", "Bearer "+tok.AccessToken)
	var drive api.DriveResource
	if _, err := srv.CallJSON(ctx, &rest.Opts{Method: "GET", Path: "/me/drive"}, nil, &drive); err != nil {
		return nil, fmt.Errorf("finding the account's OneDrive: %w", err)
	}
	if drive.DriveID == "" || drive.DriveType == "" {
		return nil, fmt.Errorf("finding the account's OneDrive: Microsoft named no drive")
	}
	return map[string]string{"drive_id": drive.DriveID, "drive_type": drive.DriveType}, nil
}
