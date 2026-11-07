package drive

import (
	"errors"
	"io"
	"strings"
	"testing"
	"time"
)

func TestReadAllStallingReadsToTheEnd(t *testing.T) {
	data, err := readAllStalling(strings.NewReader("block"), time.Second, func() {})
	if err != nil || string(data) != "block" {
		t.Fatalf("got %q, %v", data, err)
	}
}

// A connection left open without data after a network change: the watchdog cancels, which here closes the pipe like a cancelled request closes its body.
func TestReadAllStallingGivesUpWithoutData(t *testing.T) {
	r, w := io.Pipe()
	go w.Write([]byte("first bytes"))
	start := time.Now()
	_, err := readAllStalling(r, 50*time.Millisecond, func() { r.CloseWithError(errors.New("context canceled")) })
	if err == nil || !strings.Contains(err.Error(), "no data for") {
		t.Fatalf("want a stall error, got %v", err)
	}
	if time.Since(start) > 2*time.Second {
		t.Fatalf("took %v", time.Since(start))
	}
}
