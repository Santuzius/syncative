package extras

import (
	"context"
	"net"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"
)

func TestCloseAllCutsOffOpenConnections(t *testing.T) {
	set := &connSet{conns: map[*trackedConn]struct{}{}}
	client, server := net.Pipe()
	defer server.Close()
	conn := set.add(client)
	done := make(chan error)
	go func() {
		_, err := conn.Read(make([]byte, 1))
		done <- err
	}()
	if n := set.closeAll(); n != 1 {
		t.Fatalf("closed %d connections, want 1", n)
	}
	select {
	case err := <-done:
		if err == nil {
			t.Fatal("read on a closed connection succeeded")
		}
	case <-time.After(2 * time.Second):
		t.Fatal("read still hangs after closeAll")
	}
	if n := set.closeAll(); n != 0 {
		t.Fatalf("closed connections stay tracked: %d", n)
	}
}

// The event log is polled before every listing, so its requests need the same protection as the others: a network change must cut them off too.
func TestCloseConnectionsCutsOffEventRequests(t *testing.T) {
	arrived := make(chan struct{})
	release := make(chan struct{})
	srv := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
		close(arrived)
		<-release
	}))
	defer srv.Close()
	defer close(release)

	done := make(chan error)
	go func() {
		_, err := LatestVolumeEventID(context.Background(), Auth{HostURL: srv.URL}, "volume")
		done <- err
	}()
	<-arrived
	CloseConnections()
	select {
	case err := <-done:
		if err == nil {
			t.Fatal("request succeeded without an answer")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("request still hangs after CloseConnections")
	}
}
