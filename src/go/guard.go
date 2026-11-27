package main

import "C"

import (
	"encoding/json"
	"fmt"
	"os"
)

// The exports recover from panics: one that crossed into Rust would end the app. Each export defers the guard matching its reply. Panics in goroutines an export starts can't be caught here.

// panicked is the error a recovered panic becomes.
func panicked(r any) error {
	return fmt.Errorf("panic: %v", r)
}

// recoverJSON makes a panic the reply `{"error":…}`, for exports answering JSON.
func recoverJSON(out **C.char) {
	if r := recover(); r != nil {
		b, _ := json.Marshal(map[string]string{"error": panicked(r).Error()})
		*out = C.CString(string(b))
	}
}

// recoverEnvelope makes a panic the error envelope of the ProtonDrive_* exports.
func recoverEnvelope(out **C.char) {
	if r := recover(); r != nil {
		*out = errResult(panicked(r))
	}
}

// recoverMessage makes a panic the error message of an export that answers NULL or a message.
func recoverMessage(out **C.char) {
	if r := recover(); r != nil {
		*out = C.CString(panicked(r).Error())
	}
}

// recoverCount makes a panic the reply -1 of an export answering a count.
func recoverCount(n *C.int) {
	if r := recover(); r != nil {
		*n = -1
	}
}

// recoverLogged reports a panic of an export without a reply.
func recoverLogged() {
	if r := recover(); r != nil {
		fmt.Fprintln(os.Stderr, "syncative:", panicked(r))
	}
}
