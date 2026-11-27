package nextcloud

import (
	"context"
	"net/http"
	"net/http/httptest"
	"sync/atomic"
	"testing"

	"github.com/rclone/rclone/backend/webdav"
)

// TestETagRequestSurvivesADroppedConnection: a server that closes the connection without answering, as when it drops a kept-open one just as the request goes out, gets the request again.
func TestETagRequestSurvivesADroppedConnection(t *testing.T) {
	var calls atomic.Int32
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if calls.Add(1) == 1 {
			conn, _, err := w.(http.Hijacker).Hijack()
			if err == nil {
				_ = conn.Close()
			}
			return
		}
		w.WriteHeader(http.StatusMultiStatus)
		_, _ = w.Write([]byte(`<?xml version="1.0"?><d:multistatus xmlns:d="DAV:"><d:response><d:href>/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype><d:getetag>"r"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>`))
	}))
	defer srv.Close()

	tag, _, err := folderTags(context.Background(), &webdav.Options{URL: srv.URL + "/", Vendor: "nextcloud"}, "", 0)
	if err != nil || tag != `"r"` || calls.Load() != 2 {
		t.Fatalf("tag %s, err %v, %d calls", tag, err, calls.Load())
	}
}
