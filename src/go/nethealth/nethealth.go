// Package nethealth notices dead connections. After a hibernate or a network change a connection can stay open without ever delivering another byte, and a request on it waits until the kernel gives up minutes later; a timeout on the reply can't tell that apart from a server that is slow to answer, e.g. Nextcloud putting a large chunked upload together.
//
// HTTP/2 can: a live server answers a ping even while it works on a reply, a dead connection doesn't. Importing this package turns the pings on for rclone's transports, which rclone builds by copying the fields of http.DefaultTransport, and for NewTransport.
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

// shared serves http.DefaultClient and is the base of NewTransport.
var shared *http.Transport

// http.DefaultTransport becomes a template that rclone copies field by field. A transport sets up HTTP/2 once, on its first use, Clone or CloseIdleConnections, and fills in TLSNextProto; in a copy that field turns HTTP/2 off, so rclone would speak HTTP/1.1 without pings. The template is set up here and cleared again, so it stays empty whatever uses it later; requests straight through it speak HTTP/1.1, those of http.DefaultClient go through shared.
func init() {
	shared = http.DefaultTransport.(*http.Transport)
	shared.HTTP2 = Pings
	template := shared.Clone()
	template.CloseIdleConnections() // sets up HTTP/2 now, once
	template.TLSNextProto = nil
	template.TLSClientConfig.NextProtos = nil
	http.DefaultTransport = template
	http.DefaultClient.Transport = shared
}

// NewTransport returns a transport with the defaults of http.DefaultTransport and the pings.
func NewTransport() *http.Transport {
	return shared.Clone()
}
