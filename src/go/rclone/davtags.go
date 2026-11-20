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
	"strings"

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
	info, err := fs.Find("webdav")
	if err != nil {
		return "", nil, err
	}
	m := fs.ConfigMap(info.Prefix, info.Options, remote, nil)
	if kind, _ := m.Get("type"); kind != "webdav" {
		return "", nil, ErrNoFolderTags
	}
	opt := new(webdav.Options)
	if err := configstruct.Set(m, opt); err != nil {
		return "", nil, err
	}
	if opt.Vendor != "nextcloud" && opt.Vendor != "owncloud" {
		return "", nil, ErrNoFolderTags
	}
	if !strings.HasSuffix(opt.URL, "/") {
		opt.URL += "/"
	}
	if opt.Pass != "" {
		if opt.Pass, err = obscure.Reveal(opt.Pass); err != nil {
			return "", nil, fmt.Errorf("couldn't decrypt password: %w", err)
		}
	}
	return folderTags(ctx, opt, dir, depth)
}

func folderTags(ctx context.Context, opt *webdav.Options, dir string, depth int) (string, map[string]string, error) {
	endpoint, err := url.Parse(opt.URL)
	if err != nil {
		return "", nil, err
	}
	srv := rest.NewClient(fshttp.NewClient(ctx)).SetRoot(endpoint.String())
	if opt.User != "" || opt.Pass != "" {
		srv.SetUserPass(opt.User, opt.Pass)
	}
	srv.SetHeader("Referer", endpoint.String())

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
