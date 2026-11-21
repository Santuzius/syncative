package extras

// HTTP wrapper for the handful of Drive endpoints upstream
// go-proton-api doesn't expose. We can't call upstream's `Client.do()`
// from outside the package (it's unexported), and we can't add methods
// to `*proton.Client` from here either, so we run a parallel HTTP path
// (stdlib net/http, no resty) whose auth headers are populated from
// the same UID/access token Syncative already has on its own Session.
//
// This client doesn't share upstream's status observers or per-error-code handlers. It renews an expired access token through Auth.Renew, which goes through upstream's client, so a token renewed here is the session's and gets stored like any other.

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"

	"github.com/ProtonMail/go-proton-api"
)

// Auth carries the credential bits a request needs. Populate from the
// syncative/go/proton/drive Session's exposed fields. HostURL is optional;
// when blank we fall back to upstream's `proton.DefaultHostURL` so this
// package and upstream stay pinned to the same endpoint.
type Auth struct {
	UID         string
	AccessToken string
	AppVersion  string
	HostURL     string // empty → proton.DefaultHostURL
	// Renew is called once when a request is refused with 401 (an expired access token): it renews the session's tokens and returns its Auth as it is now, and the request is sent once more with it. Nil: the 401 is returned.
	Renew func(ctx context.Context) (Auth, error)
}

func (a Auth) hostURL() string {
	if a.HostURL != "" {
		return a.HostURL
	}
	return proton.DefaultHostURL
}

// httpClient is shared across calls — single transport + connection
// pool. Threaded through context-bound requests so cancellation and
// per-call timeouts behave the way callers expect.
var httpClient = &http.Client{Timeout: 90 * time.Second, Transport: NewTransport()}

// do performs an authenticated request and decodes a JSON response into
// `out` (pass nil when the response body is irrelevant). `body` is
// JSON-encoded when non-nil. On API failure the returned error is
// `*proton.APIError` so callers can use `errors.As(err,
// **proton.APIError)` the same way they do for upstream's calls.
func do(ctx context.Context, auth Auth, method, path string, body, out any) error {
	var buf []byte
	if body != nil {
		var err error
		if buf, err = json.Marshal(body); err != nil {
			return fmt.Errorf("encode body: %w", err)
		}
	}
	err := send(ctx, auth, method, path, buf, out)
	var apiErr *proton.APIError
	if auth.Renew != nil && errors.As(err, &apiErr) && apiErr.Status == http.StatusUnauthorized {
		renewed, renewErr := auth.Renew(ctx)
		if renewErr != nil {
			return renewErr
		}
		return send(ctx, renewed, method, path, buf, out)
	}
	return err
}

// send makes one request with the JSON body `buf` (nil for none).
func send(ctx context.Context, auth Auth, method, path string, buf []byte, out any) error {
	url := strings.TrimRight(auth.hostURL(), "/") + path

	var bodyReader io.Reader
	if buf != nil {
		bodyReader = bytes.NewReader(buf)
	}

	req, err := http.NewRequestWithContext(ctx, method, url, bodyReader)
	if err != nil {
		return err
	}
	if buf != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	req.Header.Set("Accept", "application/json")
	if auth.AppVersion != "" {
		req.Header.Set("x-pm-appversion", auth.AppVersion)
	}
	if auth.UID != "" {
		req.Header.Set("x-pm-uid", auth.UID)
	}
	if auth.AccessToken != "" {
		req.Header.Set("Authorization", "Bearer "+auth.AccessToken)
	}

	res, err := httpClient.Do(req)
	if err != nil {
		return err
	}
	defer res.Body.Close()

	respBody, err := io.ReadAll(res.Body)
	if err != nil {
		return err
	}

	if res.StatusCode >= 400 {
		ae := &proton.APIError{Status: res.StatusCode}
		// Upstream's APIError has a JSON tag mapping `Error` →
		// Message, so the standard unmarshal populates it correctly.
		if jerr := json.Unmarshal(respBody, ae); jerr != nil {
			return fmt.Errorf("HTTP %d: %s", res.StatusCode, strings.TrimSpace(string(respBody)))
		}
		return ae
	}

	if out != nil {
		if err := json.Unmarshal(respBody, out); err != nil {
			return fmt.Errorf("decode response: %w", err)
		}
	}
	return nil
}
