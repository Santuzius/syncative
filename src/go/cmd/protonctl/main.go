// protonctl is a second Proton Drive client for manual tests, e.g. the remote checklist in doc/development/testing.md: it changes the drive as the web app would, through a saved session.
//
//	protonctl SESSION ls PATH            the items in a folder
//	protonctl SESSION tree PATH          every path below a folder
//	protonctl SESSION put LOCAL PATH     upload a file (a new revision if it exists)
//	protonctl SESSION mkdir PATH         create a folder
//	protonctl SESSION trash PATH         move an item to the trash
//
// PATH starts at the drive's root ("/Photos/a.txt"). SESSION is a JSON file with the session (uid, access_token, refresh_token, salted_key_pass), as Syncative keeps it in the keyring under "proton-session-<remote>". A session that rotates its tokens is written back to that file. Don't use a session while Syncative uses it: a rotation by one signs the other out.
package main

import (
	"context"
	"fmt"
	"os"
	"path"
	"strings"

	"syncative/go/proton/drive"
)

func main() {
	if len(os.Args) < 4 {
		fail(fmt.Errorf("usage: protonctl SESSION ls|tree|put|mkdir|trash ARGS"))
	}
	sessionPath, cmd, args := os.Args[1], os.Args[2], os.Args[3:]
	cred, err := drive.LoadCredential(sessionPath)
	if err != nil {
		fail(err)
	}
	ctx := context.Background()
	s, err := drive.Resume(ctx, cred)
	if err != nil {
		fail(err)
	}
	err = run(ctx, s, cmd, args)
	if saved, saveErr := s.SaveIfRotated(sessionPath); saveErr != nil || saved {
		fmt.Fprintln(os.Stderr, "session rotated, saved:", saved, saveErr)
	}
	s.Close()
	if err != nil {
		fail(err)
	}
}

func run(ctx context.Context, s *drive.Session, cmd string, args []string) error {
	switch {
	case cmd == "ls" && len(args) == 1:
		id, err := linkID(ctx, s, args[0])
		if err != nil {
			return err
		}
		entries, err := s.ListDirectory(ctx, id)
		if err != nil {
			return err
		}
		for _, e := range entries {
			if e.IsDir {
				fmt.Println(e.Name + "/")
			} else {
				fmt.Println(e.Name)
			}
		}
		return nil
	case cmd == "tree" && len(args) == 1:
		id, err := linkID(ctx, s, args[0])
		if err != nil {
			return err
		}
		return tree(ctx, s, id, "")
	case cmd == "put" && len(args) == 2:
		parent, err := linkID(ctx, s, path.Dir(args[1]))
		if err != nil {
			return err
		}
		_, err = s.UploadFile(ctx, parent, path.Base(args[1]), args[0])
		return err
	case cmd == "mkdir" && len(args) == 1:
		parent, err := linkID(ctx, s, path.Dir(args[0]))
		if err != nil {
			return err
		}
		_, err = s.CreateFolder(ctx, parent, path.Base(args[0]))
		return err
	case cmd == "trash" && len(args) == 1:
		id, err := linkID(ctx, s, args[0])
		if err != nil {
			return err
		}
		return s.TrashLink(ctx, id)
	}
	return fmt.Errorf("unknown command or wrong arguments: %s %v", cmd, args)
}

// linkID finds the item at `p` by walking from the root.
func linkID(ctx context.Context, s *drive.Session, p string) (string, error) {
	id := s.RootLinkID()
	for _, name := range strings.Split(strings.Trim(p, "/"), "/") {
		if name == "" {
			continue
		}
		entries, err := s.ListDirectory(ctx, id)
		if err != nil {
			return "", err
		}
		next := ""
		for _, e := range entries {
			if e.Name == name {
				next = e.LinkID
				break
			}
		}
		if next == "" {
			return "", fmt.Errorf("%s: not found", p)
		}
		id = next
	}
	return id, nil
}

// tree prints every path below the folder `id`, each prefixed with `prefix`.
func tree(ctx context.Context, s *drive.Session, id, prefix string) error {
	entries, err := s.ListDirectory(ctx, id)
	if err != nil {
		return err
	}
	for _, e := range entries {
		fmt.Println(prefix + e.Name)
		if e.IsDir {
			if err := tree(ctx, s, e.LinkID, prefix+e.Name+"/"); err != nil {
				return err
			}
		}
	}
	return nil
}

func fail(err error) {
	fmt.Fprintln(os.Stderr, "protonctl:", err)
	os.Exit(1)
}
