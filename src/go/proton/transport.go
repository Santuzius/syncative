package extras

import (
	"context"
	"net"
	"net/http"
	"sync"
	"time"
)

// NewTransport returns the http.Transport for every request to Proton: this package's own and, via WithTransport, those of the upstream proton.Manager. Defaults from
// http.DefaultTransport leave ResponseHeaderTimeout at 0 and rely on
// the kernel's TCP keepalive (~5–12 min) to notice a dead peer, which
// surfaces as a multi-minute Syncative "Listing remote…" hang after
// suspend/resume — TCP is still alive from our side until the OS
// finally returns ENETUNREACH. The HTTP/2 ping pair forces the
// transport to actively probe an idle conn so a dead one is recycled
// before the next listing batch fans out across it.
func NewTransport() *http.Transport {
	t := http.DefaultTransport.(*http.Transport).Clone()
	dialer := &net.Dialer{
		Timeout:   30 * time.Second,
		KeepAlive: 15 * time.Second,
	}
	t.DialContext = func(ctx context.Context, network, addr string) (net.Conn, error) {
		conn, err := dialer.DialContext(ctx, network, addr)
		if err != nil {
			return nil, err
		}
		return liveConns.add(conn), nil
	}
	t.IdleConnTimeout = 30 * time.Second
	t.ResponseHeaderTimeout = 30 * time.Second
	t.HTTP2 = &http.HTTP2Config{
		SendPingTimeout: 15 * time.Second,
		PingTimeout:     15 * time.Second,
	}
	return t
}

// liveConns holds every connection the Proton transports dialed. After a network change they may stay open without ever delivering another byte, so CloseConnections closes them all.
var liveConns = &connSet{conns: map[*trackedConn]struct{}{}}

type connSet struct {
	mu    sync.Mutex
	conns map[*trackedConn]struct{}
}

type trackedConn struct {
	net.Conn
	set  *connSet
	once sync.Once
}

func (s *connSet) add(conn net.Conn) net.Conn {
	tracked := &trackedConn{Conn: conn, set: s}
	s.mu.Lock()
	s.conns[tracked] = struct{}{}
	s.mu.Unlock()
	return tracked
}

func (c *trackedConn) Close() error {
	c.once.Do(func() {
		c.set.mu.Lock()
		delete(c.set.conns, c)
		c.set.mu.Unlock()
	})
	return c.Conn.Close()
}

// closeAll closes every connection in the set; reads and writes on them fail right away. Returns how many there were.
func (s *connSet) closeAll() int {
	s.mu.Lock()
	conns := make([]*trackedConn, 0, len(s.conns))
	for c := range s.conns {
		conns = append(conns, c)
	}
	s.mu.Unlock()
	for _, c := range conns {
		c.Close()
	}
	return len(conns)
}

// CloseConnections closes every connection to Proton and returns how many there were. Requests running on them fail at once, and their retries dial anew.
func CloseConnections() int {
	return liveConns.closeAll()
}
