package rclone

// nextcloudtrash.go takes a file or folder out of Nextcloud's trash bin for the network drive's trash (trashbin.go). Nextcloud lists the trash bin at remote.php/dav/trashbin/USER/trash with each entry's original location and deletion time; moving an entry into remote.php/dav/trashbin/USER/restore puts it back where it was (https://docs.nextcloud.com/server/latest/developer_manual/client_apis/WebDAV/trashbin.html). rclone's WebDAV backend doesn't reach the trash bin, so the requests are made here, as in davtags.go.

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"net/url"
	"path"
	"strconv"
	"strings"
	"time"

	"github.com/rclone/rclone/backend/webdav"
	"github.com/rclone/rclone/lib/encoder"
	"github.com/rclone/rclone/lib/rest"
)

var trashbinProps = []byte(`<?xml version="1.0"?>
<d:propfind xmlns:d="DAV:" xmlns:nc="http://nextcloud.org/ns">
 <d:prop>
  <nc:trashbin-original-location />
  <nc:trashbin-deletion-time />
 </d:prop>
</d:propfind>
`)

type trashbinListing struct {
	Responses []struct {
		Href      string `xml:"href"`
		Propstats []struct {
			Status string `xml:"status"`
			// The namespace of a tag applies to each element of its path, so the DAV: prop is a level of its own.
			Prop struct {
				Location string `xml:"http://nextcloud.org/ns trashbin-original-location"`
				Deleted  string `xml:"http://nextcloud.org/ns trashbin-deletion-time"`
			} `xml:"DAV: prop"`
		} `xml:"propstat"`
	} `xml:"response"`
}

// NextcloudRestore takes what was at `location` (a path from the remote's root, as in rclone's listings) out of Nextcloud remote `remote`'s trash bin; of several entries from there, the one deleted closest to `deleted`. Returns ErrNoFolderTags when `remote` isn't a Nextcloud remote.
func NextcloudRestore(ctx context.Context, remote, location string, deleted time.Time) error {
	opt, err := nextcloudOptions(remote)
	if err != nil {
		return err
	}
	base, prefix, err := trashbinRoot(opt)
	if err != nil {
		return err
	}
	location = strings.Trim(path.Join(prefix, location), "/")
	if opt.Enc != encoder.EncodeZero {
		location = opt.Enc.FromStandardPath(location)
	}
	srv := davClient(ctx, opt, base)
	var listing trashbinListing
	_, err = srv.CallXML(ctx, &rest.Opts{
		Method:       "PROPFIND",
		Path:         "trash/",
		ExtraHeaders: map[string]string{"Depth": "1"},
		Body:         bytes.NewReader(trashbinProps),
		NoRedirect:   true,
	}, nil, &listing)
	if err != nil {
		return fmt.Errorf("couldn't list Nextcloud's trash bin: %w", err)
	}
	best, bestGap := "", time.Duration(-1)
	for _, item := range listing.Responses {
		for _, ps := range item.Propstats {
			if !statusOK(ps.Status) || strings.Trim(ps.Prop.Location, "/") != location {
				continue
			}
			seconds, _ := strconv.ParseInt(strings.TrimSpace(ps.Prop.Deleted), 10, 64)
			gap := time.Unix(seconds, 0).Sub(deleted).Abs()
			if bestGap < 0 || gap < bestGap {
				best, bestGap = item.Href, gap
			}
		}
	}
	if best == "" {
		return fmt.Errorf("nextcloud's trash bin holds no %s", location)
	}
	src, err := rest.URLJoin(base, best)
	if err != nil {
		return err
	}
	name := path.Base(strings.TrimSuffix(best, "/"))
	_, err = srv.Call(ctx, &rest.Opts{
		Method:  "MOVE",
		RootURL: src.String(),
		// No "Overwrite: F": Nextcloud 34 refuses every restore with it (412, "the destination node already exists"). free() has checked that the original place is free.
		ExtraHeaders: map[string]string{"Destination": base.String() + "restore/" + name},
		NoResponse:   true,
		NoRedirect:   true,
	})
	if err != nil {
		return fmt.Errorf("nextcloud couldn't restore %s: %w", location, err)
	}
	return nil
}

// trashbinRoot: the URL of the trash bin of the remote's user, and where the remote's root lies among the trash bin's original locations.
func trashbinRoot(opt *webdav.Options) (base *url.URL, prefix string, err error) {
	u, err := url.Parse(opt.URL)
	if err != nil {
		return nil, "", err
	}
	var server, user string
	if before, rest, ok := strings.Cut(u.Path, "/remote.php/dav/files/"); ok {
		var sub string
		user, sub, _ = strings.Cut(rest, "/")
		server, prefix = before, sub
	} else if before, rest, ok := strings.Cut(u.Path, "/remote.php/webdav"); ok {
		server, user, prefix = before, opt.User, rest
	} else {
		return nil, "", errors.New("not a Nextcloud WebDAV address")
	}
	base = &url.URL{Scheme: u.Scheme, User: u.User, Host: u.Host, Path: server + "/remote.php/dav/trashbin/" + user + "/"}
	return base, strings.Trim(prefix, "/"), nil
}
