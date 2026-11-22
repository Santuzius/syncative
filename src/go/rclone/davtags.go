package rclone

// davtags.go reads the ETags Nextcloud and ownCloud keep on folders. Their server gives a folder a new ETag whenever anything below it changes, up to the root, so one PROPFIND on a sync folder tells whether anything in it changed, and the ETags of its subfolders tell which branch. rclone's WebDAV backend doesn't read ETags, so the request is made here, with the remote's own options and rclone's HTTP client; the paths are built exactly as the backend builds them, so they match its listings.

import (
	"bytes"
	"context"
	"encoding/xml"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"path"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/rclone/rclone/backend/webdav"
	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/fs/config/configstruct"
	"github.com/rclone/rclone/fs/config/obscure"
	"github.com/rclone/rclone/fs/fshttp"
	"github.com/rclone/rclone/lib/encoder"
	"github.com/rclone/rclone/lib/rest"
)

// ErrNoFolderTags: the remote isn't a WebDAV remote whose server propagates folder ETags.
var ErrNoFolderTags = errors.New("the remote keeps no folder change tags")

var etagProps = []byte(`<?xml version="1.0"?>
<d:propfind xmlns:d="DAV:">
 <d:prop>
  <d:resourcetype />
  <d:getetag />
 </d:prop>
</d:propfind>
`)

type davMultistatus struct {
	Responses []struct {
		Href      string `xml:"href"`
		Propstats []struct {
			Status     string    `xml:"status"`
			Collection *xml.Name `xml:"DAV: prop>resourcetype>collection"`
			ETag       string    `xml:"DAV: prop>getetag"`
		} `xml:"propstat"`
	} `xml:"response"`
}

// FolderTags returns the ETag of folder `dir` (a path from the remote's root, as in rclone's listings) and, with `depth` 1, those of the folders directly in it by path. A missing folder gives an error that says "directory not found".
func FolderTags(ctx context.Context, remote, dir string, depth int) (tag string, children map[string]string, err error) {
	opt, err := davOptions(remote, "nextcloud", "owncloud")
	if err != nil {
		return "", nil, err
	}
	if depth != 0 {
		return folderTags(ctx, opt, dir, depth)
	}
	tag, err = gateOf(remote).folderTag(dir, func(dir string) (string, error) {
		tag, _, err := folderTags(ctx, opt, dir, 0)
		return tag, err
	})
	return tag, nil, err
}

// tagGate spares a remote's sync folders their own ETag request while nothing changed: the server gives the remote's root a new ETag with every change below it, received shares included, so a folder whose ETag was read under the root's current ETag still has it. A quiet pass then costs one request for all folders, a pass with changes one more than without the gate. The root's answer is shared like a change log's, for reuseFor.
type tagGate struct {
	sync.Mutex
	root   string
	rootAt time.Time
	// seen holds per folder its ETag and the root's ETag read just before it.
	seen map[string]seenTag
}

type seenTag struct{ tag, root string }

var (
	gatesMu sync.Mutex
	gates   = map[string]*tagGate{}
)

func gateOf(remote string) *tagGate {
	gatesMu.Lock()
	defer gatesMu.Unlock()
	g, ok := gates[remote]
	if !ok {
		g = &tagGate{seen: map[string]seenTag{}}
		gates[remote] = g
	}
	return g
}

// forgetGate drops what `remote`'s gate knows, e.g. after its config changed.
func forgetGate(remote string) {
	gatesMu.Lock()
	defer gatesMu.Unlock()
	delete(gates, remote)
}

// folderTag is the ETag of `dir`; `ask` reads one from the server.
func (g *tagGate) folderTag(dir string, ask func(string) (string, error)) (string, error) {
	g.Lock()
	defer g.Unlock()
	if dir == "" || time.Since(g.rootAt) >= reuseFor {
		root, err := ask("")
		if err != nil {
			return "", err
		}
		g.root, g.rootAt = root, time.Now()
	}
	if dir == "" {
		return g.root, nil
	}
	if seen, ok := g.seen[dir]; ok && seen.root == g.root {
		return seen.tag, nil
	}
	// The root's ETag is read before the folder's, so a change in between makes the next root differ and the folder be asked again.
	tag, err := ask(dir)
	if err != nil {
		delete(g.seen, dir)
		return "", err
	}
	g.seen[dir] = seenTag{tag: tag, root: g.root}
	return tag, nil
}

// davOptions reads the options of WebDAV remote `remote`, with the password revealed, when its vendor is one of `vendors`; else ErrNoFolderTags.
func davOptions(remote string, vendors ...string) (*webdav.Options, error) {
	info, err := fs.Find("webdav")
	if err != nil {
		return nil, err
	}
	m := fs.ConfigMap(info.Prefix, info.Options, remote, nil)
	if kind, _ := m.Get("type"); kind != "webdav" {
		return nil, ErrNoFolderTags
	}
	opt := new(webdav.Options)
	if err := configstruct.Set(m, opt); err != nil {
		return nil, err
	}
	if !slices.Contains(vendors, opt.Vendor) {
		return nil, ErrNoFolderTags
	}
	if !strings.HasSuffix(opt.URL, "/") {
		opt.URL += "/"
	}
	if opt.Pass != "" {
		if opt.Pass, err = obscure.Reveal(opt.Pass); err != nil {
			return nil, fmt.Errorf("couldn't decrypt password: %w", err)
		}
	}
	return opt, nil
}

// davHTTP is the HTTP client of every ETag request. One for all keeps its connections open between passes: a client of its own per request would dial and shake hands anew each time, and leave the old connection idle until the server closes it.
var davHTTP = sync.OnceValue(func() *http.Client { return fshttp.NewClient(context.Background()) })

// davClient is rclone's HTTP client for `endpoint`, signed in as the remote's user.
func davClient(ctx context.Context, opt *webdav.Options, endpoint *url.URL) *rest.Client {
	srv := rest.NewClient(davHTTP()).SetRoot(endpoint.String())
	if opt.User != "" || opt.Pass != "" {
		srv.SetUserPass(opt.User, opt.Pass)
	}
	srv.SetHeader("Referer", endpoint.String())
	return srv
}

func folderTags(ctx context.Context, opt *webdav.Options, dir string, depth int) (string, map[string]string, error) {
	endpoint, err := url.Parse(opt.URL)
	if err != nil {
		return "", nil, err
	}
	srv := davClient(ctx, opt, endpoint)

	// As the backend's dirPath: the encoded, escaped path with a slash at the end.
	subPath := strings.Trim(dir, "/")
	if opt.Enc != encoder.EncodeZero {
		subPath = opt.Enc.FromStandardPath(subPath)
	}
	dirPath := rest.URLPathEscape(subPath)
	if dirPath != "" && !strings.HasSuffix(dirPath, "/") {
		dirPath += "/"
	}
	opts := rest.Opts{
		Method:       "PROPFIND",
		Path:         dirPath,
		ExtraHeaders: map[string]string{"Depth": fmt.Sprint(depth)},
		Body:         bytes.NewReader(etagProps),
		NoRedirect:   true,
	}
	var result davMultistatus
	resp, err := srv.CallXML(ctx, &opts, nil, &result)
	if err != nil {
		if resp != nil && resp.StatusCode == http.StatusNotFound {
			return "", nil, fs.ErrorDirNotFound
		}
		return "", nil, fmt.Errorf("couldn't read the folder's ETags: %w", err)
	}

	baseURL, err := rest.URLJoin(endpoint, opts.Path)
	if err != nil {
		return "", nil, err
	}
	dir = strings.Trim(dir, "/")
	tag, found := "", false
	children := map[string]string{}
	for _, item := range result.Responses {
		var etag string
		isDir, ok := false, false
		for _, ps := range item.Propstats {
			if !statusOK(ps.Status) {
				continue
			}
			ok = true
			if ps.Collection != nil && ps.Collection.Space == "DAV:" && ps.Collection.Local == "collection" {
				isDir = true
			}
			if ps.ETag != "" {
				etag = ps.ETag
			}
		}
		if !ok || !isDir {
			continue
		}
		// As the backend's listAll turns an href into a remote path.
		u, err := rest.URLJoin(baseURL, item.Href)
		if err != nil {
			continue
		}
		if !strings.HasSuffix(u.Path, "/") {
			u.Path += "/"
		}
		if !strings.HasPrefix(u.Path, baseURL.Path) {
			continue
		}
		sub := strings.TrimPrefix(u.Path[len(baseURL.Path):], "/")
		if opt.Enc != encoder.EncodeZero {
			sub = opt.Enc.ToStandardPath(sub)
		}
		remotePath := strings.TrimSuffix(path.Join(dir, sub), "/")
		if remotePath == "." {
			remotePath = ""
		}
		if etag == "" {
			return "", nil, fmt.Errorf("the server sent no ETag for %q", remotePath)
		}
		if remotePath == dir {
			tag, found = etag, true
			continue
		}
		children[remotePath] = etag
	}
	if !found {
		return "", nil, fmt.Errorf("the server sent no ETag for %q", dir)
	}
	return tag, children, nil
}

// statusOK reads "HTTP/1.1 200 OK"; no status counts as OK, as in the backend.
func statusOK(status string) bool {
	if status == "" {
		return true
	}
	fields := strings.Fields(status)
	return len(fields) >= 2 && strings.HasPrefix(fields[1], "2")
}
