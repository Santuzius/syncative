package drive

// Fetching a file revision's blocks for Open (nodes.go), ported from Proton-API-Bridge/file_download.go. `getRevisionAllBlocks` pages through `Client.GetRevision` to collect every block of a revision; Bridge relied on a flattened view that upstream doesn't expose. Block signatures are checked with the default address keyring, the single-account assumption folder.go makes too: Proton's block signatures are owner-signed.

import (
	"context"
	"errors"
	"fmt"
	"io"
	"sync/atomic"
	"time"

	"github.com/ProtonMail/go-proton-api"
)

// Errors specific to the download path.
var (
	ErrLinkIsNotFile    = errors.New("link is not a file")
	ErrNoActiveRevision = errors.New("file has no active revision")
)

// getRevisionPageSize is how many blocks we request per `GetRevision`
// call. Proton's API doesn't document a maximum explicitly; Bridge's
// fork pins 500. Leave generous headroom and we'll tune if we hit
// server-side limits.
const getRevisionPageSize = 500

// blockStallTimeout and blockRetryDelays bound how a block download rides out a network change; see fetchBlock. Right after the change the new network may refuse connections for a few seconds, e.g. while a VPN app rebuilds its tunnel, so the attempts spread over about a minute.
var (
	blockStallTimeout = 30 * time.Second
	blockRetryDelays  = []time.Duration{time.Second, 5 * time.Second, 15 * time.Second, 30 * time.Second}
)

// getRevisionAllBlocks returns a Revision with every block populated,
// paginated by `GetRevision`. Upstream go-proton-api only exposes
// paged access — this is the helper Bridge had built into its own
// API surface that we have to reconstruct.
func (s *Session) getRevisionAllBlocks(
	ctx context.Context,
	linkID, revisionID string,
) (proton.Revision, error) {
	var full proton.Revision
	fromBlock := 1
	for {
		page, err := s.c.GetRevision(ctx, s.mainShare.ShareID, linkID, revisionID, fromBlock, getRevisionPageSize)
		if err != nil {
			return proton.Revision{}, err
		}
		if fromBlock == 1 {
			full = page
			full.Blocks = append([]proton.Block(nil), page.Blocks...)
		} else {
			full.Blocks = append(full.Blocks, page.Blocks...)
		}
		if len(page.Blocks) < getRevisionPageSize {
			break
		}
		fromBlock += len(page.Blocks)
	}
	return full, nil
}

// fetchBlock downloads one encrypted block. A network change cuts the connection mid-block ("unexpected EOF") or leaves it open without data, so a block gets several attempts with growing pauses, each given up after blockStallTimeout without a byte.
func (s *Session) fetchBlock(ctx context.Context, block *proton.Block) ([]byte, error) {
	attempts := len(blockRetryDelays) + 1
	for attempt := 1; ; attempt++ {
		data, err := s.fetchBlockOnce(ctx, block)
		if err == nil || ctx.Err() != nil {
			return data, err
		}
		logf("drive: block %d, attempt %d of %d: %v", block.Index, attempt, attempts, err)
		if attempt == attempts {
			return nil, err
		}
		select {
		case <-time.After(blockRetryDelays[attempt-1]):
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}
}

func (s *Session) fetchBlockOnce(ctx context.Context, block *proton.Block) ([]byte, error) {
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	body, err := s.c.GetBlock(ctx, block.BareURL, block.Token)
	if err != nil {
		return nil, err
	}
	defer body.Close()
	return readAllStalling(body, blockStallTimeout, cancel)
}

// readAllStalling reads r to the end like io.ReadAll, but calls cancel, which must abort the read, once timeout passes without a byte.
func readAllStalling(r io.Reader, timeout time.Duration, cancel func()) ([]byte, error) {
	var stalled atomic.Bool
	timer := time.AfterFunc(timeout, func() {
		stalled.Store(true)
		cancel()
	})
	defer timer.Stop()
	data, err := io.ReadAll(&progressReader{r: r, progress: func() { timer.Reset(timeout) }})
	if err != nil && stalled.Load() {
		return nil, fmt.Errorf("no data for %v", timeout)
	}
	return data, err
}

type progressReader struct {
	r        io.Reader
	progress func()
}

func (p *progressReader) Read(b []byte) (int, error) {
	n, err := p.r.Read(b)
	if n > 0 {
		p.progress()
	}
	return n, err
}
