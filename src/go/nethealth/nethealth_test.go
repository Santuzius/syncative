package nethealth_test

import (
	"context"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"
	"time"

	"syncative/go/nethealth"

	"github.com/rclone/rclone/fs/fshttp"
)

// rcloneClient is an rclone HTTP client that trusts the test server's certificate.
func rcloneClient(server *httptest.Server) *http.Client {
	return &http.Client{Transport: fshttp.NewTransportCustom(context.Background(), func(t *http.Transport) {
		t.TLSClientConfig = server.Client().Transport.(*http.Transport).TLSClientConfig.Clone()
		t.TLSClientConfig.NextProtos = []string{"h2"}
		if t.HTTP2 != nethealth.Pings {
			panic("rclone's transport has no pings")
		}
	})}
}

func h2Server(handler http.HandlerFunc) *httptest.Server {
	server := httptest.NewUnstartedServer(handler)
	server.EnableHTTP2 = true
	server.StartTLS()
	return server
}

// freezer forwards TCP to `target` until frozen; then it keeps the connections open but passes nothing on, like a connection cut off by a hibernate.
type freezer struct {
	listener net.Listener
	mu       sync.Mutex
	frozen   bool
}

func newFreezer(t *testing.T, target string) *freezer {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	f := &freezer{listener: listener}
	go func() {
		for {
			client, err := listener.Accept()
			if err != nil {
				return
			}
			server, err := net.Dial("tcp", target)
			if err != nil {
				client.Close()
				continue
			}
			go f.pipe(client, server)
			go f.pipe(server, client)
		}
	}()
	t.Cleanup(func() { listener.Close() })
	return f
}

func (f *freezer) pipe(from, to net.Conn) {
	buf := make([]byte, 32<<10)
	for {
		n, err := from.Read(buf)
		f.mu.Lock()
		frozen := f.frozen
		f.mu.Unlock()
		if frozen {
			continue // swallowed
		}
		if n > 0 {
			to.Write(buf[:n])
		}
		if err != nil {
			to.Close()
			return
		}
	}
}

func (f *freezer) freeze() {
	f.mu.Lock()
	f.frozen = true
	f.mu.Unlock()
}

func TestASilentConnectionFailsWithinHalfAMinute(t *testing.T) {
	if testing.Short() {
		t.Skip("waits for the pings")
	}
	var f *freezer
	server := h2Server(func(w http.ResponseWriter, r *http.Request) {
		f.freeze() // the request reached the server, the connection dies before the reply
		time.Sleep(2 * time.Second)
		io.WriteString(w, "late")
	})
	defer server.Close()
	f = newFreezer(t, server.Listener.Addr().String())
	client := rcloneClient(server)
	start := time.Now()
	resp, err := client.Get("https://" + f.listener.Addr().String() + "/")
	if err == nil {
		resp.Body.Close()
		t.Fatal("the request succeeded over a dead connection")
	}
	if elapsed := time.Since(start); elapsed > 40*time.Second {
		t.Fatalf("noticed the dead connection after %v", elapsed)
	} else {
		t.Logf("failed after %v: %v", elapsed.Round(time.Second), err)
	}
}

func TestASlowReplyOnALiveConnectionArrives(t *testing.T) {
	if testing.Short() {
		t.Skip("waits longer than the pings")
	}
	server := h2Server(func(w http.ResponseWriter, r *http.Request) {
		time.Sleep(45 * time.Second) // longer than both ping timeouts together
		io.WriteString(w, "done")
	})
	defer server.Close()
	resp, err := rcloneClient(server).Get(server.URL)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)
	if resp.ProtoMajor != 2 || string(body) != "done" {
		t.Fatalf("got %s %q", resp.Proto, body)
	}
}
