package main

/*
#include <stdlib.h>
*/
import "C"

import (
	"context"
	"encoding/json"
	"os"
	"reflect"
	"time"

	"syncative/go/proton/drive"
	"syncative/go/proton/rclonefs"
	"syncative/go/rclone/changes"
	"syncative/go/rclone/netdrive"

	"github.com/ProtonMail/go-proton-api"
	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/fs/cache"
)

// Calls are shaped as "takes one JSON string, returns one JSON string".
// The Rust side marshals arguments into a small request struct and
// reads the return value as a result envelope — see
// `src/go/src/lib.rs`. Caller is responsible for freeing every
// returned *C.char with RcloneFreeString (same Go allocator).
//
// Error handling: any result that's not an outright success returns
// a JSON object with an `error` field describing what went wrong.
// Crashes in Go are converted to error strings — the FFI boundary
// never panics.

// result is the envelope every ProtonDrive_* export returns as JSON.
type result struct {
	OK    bool            `json:"ok"`
	Data  json.RawMessage `json:"data,omitempty"`
	Error string          `json:"error,omitempty"`
}

// okResult returns a JSON success envelope; `data` can be nil.
//
// Typed-nil collections (a Go gotcha: a non-nil interface{} wrapping a
// nil slice/map) are normalised to "[]" / "{}" before marshalling. The
// Rust side parses Envelope.data as Option<serde_json::Value>, where a
// JSON `null` deserialises to None — same as a missing field — and
// any caller using `call_json::<_, Vec<T>>` then bails with "shim
// returned OK but no data to deserialise". Emitting an empty
// collection instead keeps that path honest.
func okResult(data any) *C.char {
	envelope := result{OK: true}
	if data != nil {
		b, err := json.Marshal(data)
		if err != nil {
			return errResult(err)
		}
		switch v := reflect.ValueOf(data); {
		case v.Kind() == reflect.Slice && v.IsNil():
			b = []byte("[]")
		case v.Kind() == reflect.Map && v.IsNil():
			b = []byte("{}")
		}
		envelope.Data = b
	}
	b, _ := json.Marshal(envelope)
	return C.CString(string(b))
}

// errResult converts a Go error into the JSON error envelope the Rust
// side consumes.
func errResult(err error) *C.char {
	envelope := result{OK: false, Error: err.Error()}
	b, _ := json.Marshal(envelope)
	return C.CString(string(b))
}

// ProtonDrive_Version returns a short identity string proving the
// go-proton-api dependency linked into our archive. Caller must free
// the result with RcloneFreeString (same allocator).
//
//export ProtonDrive_Version
func ProtonDrive_Version() *C.char {
	// Build a fresh Manager just to exercise the symbol path; we throw
	// it away immediately. No network traffic.
	_ = proton.New(proton.WithAppVersion(drive.AppVersion))
	return C.CString("syncative-go proton-api bound")
}

// ProtonDrive_SetAppVersion sets the `x-pm-appversion` header sent by every later session. Input `{"version":"..."}`; call once at startup, before any login or resume.
//
//export ProtonDrive_SetAppVersion
func ProtonDrive_SetAppVersion(paramsJSON *C.char) *C.char {
	var p struct {
		Version string `json:"version"`
	}
	if err := json.Unmarshal([]byte(C.GoString(paramsJSON)), &p); err != nil {
		return errResult(err)
	}
	drive.SetAppVersion(p.Version)
	return okResult(nil)
}

// ProtonDrive_ResetConnections closes every connection to Proton after a network change, so cut-off requests fail at once and retry over the new network.
//
//export ProtonDrive_ResetConnections
func ProtonDrive_ResetConnections() {
	drive.ResetConnections()
}

// ProtonDrive_Login performs a full username+password (+ optional
// TOTP / mailbox password) login. Input JSON follows
// `drive.LoginParams`; on success the result data is a
// `drive.ReusableCredential` (UID, tokens, salted key pass). The
// session is registered in-process so subsequent calls can reference
// it by UID.
//
//export ProtonDrive_Login
func ProtonDrive_Login(paramsJSON *C.char) *C.char {
	var p drive.LoginParams
	if err := json.Unmarshal([]byte(C.GoString(paramsJSON)), &p); err != nil {
		return errResult(err)
	}
	sess, err := drive.Login(context.Background(), p)
	if err != nil {
		return errResult(err)
	}
	drive.Register(sess)
	return okResult(sess.AsCredential())
}

// ProtonDrive_BindRemote makes the rclone remote `remote` reach Proton through the registered session `uid` (backend rclonefs). Input `{"uid":"...","remote":"..."}`. The remote is defined through the environment (RCLONE_CONFIG_<NAME>_TYPE), so it never enters the rclone config file, which holds no Proton credentials either.
//
//export ProtonDrive_BindRemote
func ProtonDrive_BindRemote(paramsJSON *C.char) *C.char {
	var p struct {
		UID    string `json:"uid"`
		Remote string `json:"remote"`
	}
	if err := json.Unmarshal([]byte(C.GoString(paramsJSON)), &p); err != nil {
		return errResult(err)
	}
	if _, err := drive.Lookup(p.UID); err != nil {
		return errResult(err)
	}
	if err := os.Setenv(fs.ConfigToEnv(p.Remote, "type"), rclonefs.TypeName); err != nil {
		return errResult(err)
	}
	before := drive.BindRemote(p.Remote, p.UID)
	if before == "" || before == p.UID {
		return okResult(map[string]string{})
	}
	// Another sign-in, possibly another account: its folder IDs start over. Network drives keep the backend they were mounted with, on the replaced session, until they are mounted again; the caller then ends that session (ProtonDrive_Logout).
	dropBackend(p.Remote)
	return okResult(map[string]string{"replaced": before})
}

// ProtonDrive_UnbindRemote undoes ProtonDrive_BindRemote, e.g. when the remote is removed. Input `{"remote":"..."}`.
//
//export ProtonDrive_UnbindRemote
func ProtonDrive_UnbindRemote(paramsJSON *C.char) *C.char {
	var p struct {
		Remote string `json:"remote"`
	}
	if err := json.Unmarshal([]byte(C.GoString(paramsJSON)), &p); err != nil {
		return errResult(err)
	}
	uid := drive.UnbindRemote(p.Remote)
	dropBackend(p.Remote)
	_ = os.Unsetenv(fs.ConfigToEnv(p.Remote, "type"))
	// The remote is gone, so is its sign-in: revoke the session, as signing out does.
	if uid != "" {
		_ = logout(uid)
	}
	return okResult(nil)
}

// logoutTimeout bounds revoking a session on Proton's side; offline, it stays valid there until it expires.
const logoutTimeout = 15 * time.Second

// logout revokes the session `uid` on Proton's side and drops it here, also when revoking fails.
func logout(uid string) error {
	sess, err := drive.Lookup(uid)
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), logoutTimeout)
	defer cancel()
	err = sess.Logout(ctx)
	drive.Unregister(uid)
	return err
}

// dropBackend removes `remote`'s backend from rclone's cache, with the network drive wrappers holding it, so the next call creates it anew.
func dropBackend(remote string) {
	changes.Forget(remote)
	cache.ClearConfig(remote)
	netdrive.ForgetBackends(remote)
}

// ProtonDrive_Logout revokes the session on Proton's side and tears
// down the local state. Input JSON is `{"uid": "..."}`.
//
//export ProtonDrive_Logout
func ProtonDrive_Logout(paramsJSON *C.char) *C.char {
	var p struct {
		UID string `json:"uid"`
	}
	if err := json.Unmarshal([]byte(C.GoString(paramsJSON)), &p); err != nil {
		return errResult(err)
	}
	if err := logout(p.UID); err != nil {
		return errResult(err)
	}
	return okResult(nil)
}

// ProtonDrive_SaveSession serializes the named session's reusable
// credential to `path`. Input JSON is `{"uid":"...","path":"..."}`.
//
//export ProtonDrive_SaveSession
func ProtonDrive_SaveSession(paramsJSON *C.char) *C.char {
	var p struct {
		UID  string `json:"uid"`
		Path string `json:"path"`
	}
	if err := json.Unmarshal([]byte(C.GoString(paramsJSON)), &p); err != nil {
		return errResult(err)
	}
	sess, err := drive.Lookup(p.UID)
	if err != nil {
		return errResult(err)
	}
	if err := sess.Save(p.Path); err != nil {
		return errResult(err)
	}
	return okResult(nil)
}

// ProtonDrive_SaveSessionIfRotated writes the session blob to `path`
// only when the access/refresh tokens have rotated since the last
// save, so the caller can cheaply poll after each sync pass and skip
// the keyring write when nothing changed. Input JSON is
// `{"uid":"...","path":"..."}`; result data is `{"saved": bool}`.
//
//export ProtonDrive_SaveSessionIfRotated
func ProtonDrive_SaveSessionIfRotated(paramsJSON *C.char) *C.char {
	var p struct {
		UID  string `json:"uid"`
		Path string `json:"path"`
	}
	if err := json.Unmarshal([]byte(C.GoString(paramsJSON)), &p); err != nil {
		return errResult(err)
	}
	sess, err := drive.Lookup(p.UID)
	if err != nil {
		return errResult(err)
	}
	saved, err := sess.SaveIfRotated(p.Path)
	if err != nil {
		return errResult(err)
	}
	return okResult(struct {
		Saved bool `json:"saved"`
	}{Saved: saved})
}

// ProtonDrive_NextRotation blocks until the tokens of a session were renewed and returns `{"uid":"...","remotes":[...]}` (drive.NextRotation). Syncative calls it in a loop from a thread of its own and stores the session each time. Input `{}`.
//
//export ProtonDrive_NextRotation
func ProtonDrive_NextRotation(paramsJSON *C.char) *C.char {
	return okResult(drive.NextRotation())
}

// ProtonDrive_ResumeSession reads a saved credential blob from disk
// and rehydrates a session. Input JSON is `{"path":"..."}`; result
// data is the `drive.ReusableCredential` of the resumed session so
// the caller can pick up the UID.
//
//export ProtonDrive_ResumeSession
func ProtonDrive_ResumeSession(paramsJSON *C.char) *C.char {
	var p struct {
		Path string `json:"path"`
	}
	if err := json.Unmarshal([]byte(C.GoString(paramsJSON)), &p); err != nil {
		return errResult(err)
	}
	cred, err := drive.LoadCredential(p.Path)
	if err != nil {
		return errResult(err)
	}
	sess, err := drive.Resume(context.Background(), cred)
	if err != nil {
		return errResult(err)
	}
	drive.Register(sess)
	return okResult(sess.AsCredential())
}
