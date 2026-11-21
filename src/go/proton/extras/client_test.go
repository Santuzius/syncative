package extras

import (
	"context"
	"errors"
	"io"
	"math/rand"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/ProtonMail/go-proton-api"
)

// A server that accepts only the access token "new", like Proton after the old one expired.
func expiredTokenServer(t *testing.T, bodies *[]string) *httptest.Server {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		*bodies = append(*bodies, string(body))
		if r.Header.Get("Authorization") != "Bearer new" {
			w.WriteHeader(http.StatusUnauthorized)
			_, _ = w.Write([]byte(`{"Code":401,"Error":"Invalid access token"}`))
			return
		}
		_, _ = w.Write([]byte(`{"Code":1000,"EventID":"e1"}`))
	}))
	t.Cleanup(server.Close)
	return server
}

// The change check that starts every pass goes through this package; an expired access token must be renewed here, not end the session.
func TestExpiredAccessTokenIsRenewedOnce(t *testing.T) {
	var bodies []string
	server := expiredTokenServer(t, &bodies)
	renewals := 0
	auth := Auth{UID: "u", AccessToken: "old", HostURL: server.URL}
	auth.Renew = func(context.Context) (Auth, error) {
		renewals++
		return Auth{UID: "u", AccessToken: "new", HostURL: server.URL}, nil
	}

	id, err := LatestVolumeEventID(context.Background(), auth, "v")
	if err != nil || id != "e1" {
		t.Fatalf("got %q, %v; want e1", id, err)
	}
	if renewals != 1 {
		t.Fatalf("renewed %d times, want 1", renewals)
	}

	// A request with a body sends it again in full.
	bodies = nil
	if err := RenameLink(context.Background(), auth, "v", "l", RenameLinkReq{Name: "n"}); err != nil {
		t.Fatal(err)
	}
	if len(bodies) != 2 || bodies[0] == "" || bodies[0] != bodies[1] {
		t.Fatalf("bodies sent: %q", bodies)
	}
}

func TestWithoutRenewThe401IsReturned(t *testing.T) {
	var bodies []string
	server := expiredTokenServer(t, &bodies)
	_, err := LatestVolumeEventID(context.Background(), Auth{AccessToken: "old", HostURL: server.URL}, "v")
	var apiErr *proton.APIError
	if !errors.As(err, &apiErr) || apiErr.Status != http.StatusUnauthorized {
		t.Fatalf("got %v, want a 401", err)
	}
}

func TestFailedRenewalIsReturned(t *testing.T) {
	var bodies []string
	server := expiredTokenServer(t, &bodies)
	offline := errors.New("network is unreachable")
	auth := Auth{AccessToken: "old", HostURL: server.URL}
	auth.Renew = func(context.Context) (Auth, error) { return Auth{}, offline }
	if _, err := LatestVolumeEventID(context.Background(), auth, "v"); !errors.Is(err, offline) {
		t.Fatalf("got %v, want the renewal's error", err)
	}
}

// A walk of a large tree runs into Proton's rate limit; the request waits as Retry-After asks and is sent again instead of failing the listing.
func TestTooManyRequestsIsRetried(t *testing.T) {
	retryJitter = func() time.Duration { return 0 }
	t.Cleanup(func() { retryJitter = func() time.Duration { return time.Duration(rand.Intn(10)) * time.Second } })
	requests := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests++
		if requests < 3 {
			w.Header().Set("Retry-After", "0")
			w.WriteHeader(http.StatusTooManyRequests)
			_, _ = w.Write([]byte(`{"Code":2028,"Error":"Too many recent API requests"}`))
			return
		}
		_, _ = w.Write([]byte(`{"Code":1000,"EventID":"e1"}`))
	}))
	t.Cleanup(server.Close)
	var out struct{ EventID string }
	if err := do(context.Background(), Auth{UID: "u", AccessToken: "a", HostURL: server.URL}, "GET", "/x", nil, &out); err != nil {
		t.Fatalf("do: %v", err)
	}
	if requests != 3 || out.EventID != "e1" {
		t.Fatalf("requests = %d, EventID = %q; want 3, e1", requests, out.EventID)
	}
}

// After busyRetries refusals the 429 is returned.
func TestTooManyRequestsGivesUp(t *testing.T) {
	retryJitter = func() time.Duration { return 0 }
	t.Cleanup(func() { retryJitter = func() time.Duration { return time.Duration(rand.Intn(10)) * time.Second } })
	requests := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests++
		w.Header().Set("Retry-After", "0")
		w.WriteHeader(http.StatusTooManyRequests)
		_, _ = w.Write([]byte(`{"Code":2028,"Error":"Too many recent API requests"}`))
	}))
	t.Cleanup(server.Close)
	err := do(context.Background(), Auth{UID: "u", AccessToken: "a", HostURL: server.URL}, "GET", "/x", nil, nil)
	var apiErr *proton.APIError
	if !errors.As(err, &apiErr) || apiErr.Status != http.StatusTooManyRequests || requests != busyRetries+1 {
		t.Fatalf("err = %v, requests = %d", err, requests)
	}
}
