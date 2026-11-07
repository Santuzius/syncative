// Syncative's combined Go archive. Exposes both rclone's librclone RPC surface (rclone.go, replacing the upstream librclone-sys crate) and the C entry points of the native Proton Drive client (proton.go).
//
// Built as:
//
//	go build --buildmode=c-archive -o libsyncative_go.a .
//
// One Go runtime, both feature sets: two independent Go archives can't coexist in the Syncative binary.
package main

// main is required by cgo for c-archive builds; body intentionally empty.
func main() {}
