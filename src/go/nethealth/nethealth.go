// Package nethealth notices dead connections. After a hibernate or a network change a connection can stay open without ever delivering another byte, and a request on it waits until the kernel gives up minutes later; a timeout on the reply can't tell that apart from a server that is slow to answer, e.g. Nextcloud putting a large chunked upload together.
//
// HTTP/2 can: a live server answers a ping even while it works on a reply, a dead connection doesn't. Importing this package turns the pings on for every transport copied from http.DefaultTransport, which includes all of rclone's.
package nethealth

import (
	"net/http"
	"time"
)

// Pings closes an HTTP/2 connection that went silent: after 15 s without a frame the client pings, and without an answer within 15 s it closes the connection, so the requests on it fail and their retries dial anew. A slow reply on a live connection keeps waiting. HTTP/1.1 has no pings.
var Pings = &http.HTTP2Config{
	SendPingTimeout: 15 * time.Second,
	PingTimeout:     15 * time.Second,
}

// rclone builds its transports as copies of the default one, pings included.
func init() {
	http.DefaultTransport.(*http.Transport).HTTP2 = Pings
}
