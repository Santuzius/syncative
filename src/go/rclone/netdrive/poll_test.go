package netdrive

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"

	_ "github.com/rclone/rclone/backend/webdav"
	"github.com/rclone/rclone/fs/rc"
)

// TestAFailedForgetIsRepeated: when the VFS couldn't forget the folders that changed, the next poll finds them changed again.
func TestAFailedForgetIsRepeated(t *testing.T) {
	var mu sync.Mutex
	tags := map[string]string{"/": `"r1"`, "/sub/": `"s1"`}
	answer := func(hrefs ...string) string {
		body := `<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">`
		for _, h := range hrefs {
			body += fmt.Sprintf(`<d:response><d:href>%s</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype><d:getetag>%s</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>`, h, tags[h])
		}
		return body + `</d:multistatus>`
	}
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		w.WriteHeader(http.StatusMultiStatus)
		if r.URL.Path == "/" {
			_, _ = w.Write([]byte(answer("/", "/sub/")))
		} else {
			_, _ = w.Write([]byte(answer("/sub/")))
		}
	}))
	defer srv.Close()
	t.Setenv("RCLONE_CONFIG_POLLTESTNC_TYPE", "webdav")
	t.Setenv("RCLONE_CONFIG_POLLTESTNC_URL", srv.URL+"/")
	t.Setenv("RCLONE_CONFIG_POLLTESTNC_VENDOR", "nextcloud")

	failing := true
	var forgotten []rc.Params
	forget := &rc.Call{Fn: func(_ context.Context, in rc.Params) (rc.Params, error) {
		if failing {
			return nil, errors.New("forget failed")
		}
		forgotten = append(forgotten, in)
		return rc.Params{}, nil
	}}
	state := &mountState{tags: map[string]string{}}
	poll := func() (int, error) {
		n, _, err := pollFolderTags(context.Background(), forget, "polltestnc:", "polltestnc", "", state)
		return n, err
	}

	if _, err := poll(); err != nil {
		t.Fatal(err)
	}
	mu.Lock()
	tags["/"], tags["/sub/"] = `"r2"`, `"s2"`
	mu.Unlock()
	if _, err := poll(); err == nil {
		t.Fatal("the failed forget went unreported")
	}
	failing = false
	n, err := poll()
	if err != nil || n != 2 || len(forgotten) != 1 {
		t.Fatalf("after the failed forget: %d folders changed, forgotten %v, %v; want both folders again", n, forgotten, err)
	}
}
