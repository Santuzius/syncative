package drive

import (
	"context"
)

// Quota reports the account's storage, in bytes. A Proton plan is one
// pool shared by Mail, Drive and Calendar, so these are the same
// account-wide numbers the web app shows in its sidebar — not a
// Drive-only figure.
type Quota struct {
	Used uint64 `json:"used"`
	Max  uint64 `json:"max"`
}

// Quota fetches the numbers via the core API's user endpoint.
func (s *Session) Quota(ctx context.Context) (*Quota, error) {
	user, err := s.c.GetUser(ctx)
	if err != nil {
		return nil, err
	}
	return &Quota{Used: user.UsedSpace, Max: user.MaxSpace}, nil
}
