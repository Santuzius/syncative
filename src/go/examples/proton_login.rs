//! Live Proton login, then a listing of the drive's root through rclone.
//!
//! Reads credentials from env vars — keeps them out of shell history
//! and argv — then exercises: login → save → resume → list the root
//! through rclone → logout. For changing the drive, see
//! `cmd/protonctl`.
//!
//!     PROTON_USERNAME=alice@proton.me \
//!     PROTON_PASSWORD='…' \
//!     PROTON_TOTP=123456 \            # optional
//!     PROTON_MAILBOX_PASSWORD='…' \   # optional, two-password mode only
//!     nix-shell --run "cargo run --manifest-path src/go/Cargo.toml \
//!         --example proton_login"
//!
//! On success you should see a UID, the saved credential file path,
//! a resumed UID (should match), and a clean logout line. Any error
//! surfaces as an `Err(...)` from Rust — Go side can't crash the
//! process across the FFI boundary.

use std::{env, path::PathBuf};

use syncative_go::{self as native, proton};

fn main() {
    // One-time Go runtime + rclone/native init.
    native::initialize();
    eprintln!("identity: {}", native::proton_drive_version());

    let params = proton::LoginParams {
        username: require("PROTON_USERNAME"),
        password: require("PROTON_PASSWORD"),
        two_fa: env::var("PROTON_TOTP").unwrap_or_default(),
        mailbox_password: env::var("PROTON_MAILBOX_PASSWORD").unwrap_or_default(),
        ..Default::default()
    };

    eprintln!("logging in as {}…", mask(&params.username));
    let cred = match proton::login(&params) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("login failed: {err}");
            std::process::exit(1);
        }
    };
    eprintln!("  login OK — uid={}", cred.uid);

    let session_path: PathBuf = env::temp_dir().join(format!(
        "syncative-proton-session-smoke-{}.json",
        std::process::id()
    ));
    if let Err(err) = proton::save_session(&cred.uid, &session_path) {
        eprintln!("save_session failed: {err}");
        // still attempt logout
        let _ = proton::logout(&cred.uid);
        std::process::exit(1);
    }
    eprintln!("  saved session → {}", session_path.display());

    // Resume into a *second* registered session from the same blob —
    // proves save / load works without touching the live creds again.
    let resumed = match proton::resume_session(&session_path) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("resume_session failed: {err}");
            let _ = proton::logout(&cred.uid);
            std::process::exit(1);
        }
    };
    eprintln!(
        "  resumed — uid={} (matches? {})",
        resumed.uid,
        resumed.uid == cred.uid
    );

    // List the drive's root through rclone, the way the sync reaches it: the session is bound to a remote name, which rclone's "native-proton" backend looks up.
    if let Err(err) = proton::bind_remote(&resumed.uid, "smoke") {
        eprintln!("bind_remote failed: {err}");
        let _ = proton::logout(&resumed.uid);
        std::process::exit(1);
    }
    match native::rpc("operations/list", r#"{"fs":"smoke:","remote":""}"#) {
        Ok(listing) => eprintln!("  root listing: {listing}"),
        Err(err) => eprintln!("  listing failed: {err}"),
    }
    proton::unbind_remote("smoke");

    match proton::logout(&resumed.uid) {
        Ok(()) => eprintln!("  logout OK"),
        Err(err) => eprintln!("  logout returned: {err}"),
    }
    let _ = std::fs::remove_file(&session_path);

    // The original (first) session is now also invalidated upstream
    // since it shared the refresh token. No need to log it out again.
    eprintln!("done.");
}

fn require(var: &str) -> String {
    match env::var(var) {
        Ok(v) if !v.is_empty() => v,
        _ => {
            eprintln!("{var} is required (export it before running)");
            std::process::exit(2);
        }
    }
}

/// Redact all but the first character and the domain so the log shows
/// enough to identify the account while keeping the full email out of
/// terminal history.
fn mask(username: &str) -> String {
    let (local, at, domain) = match username.find('@') {
        Some(i) => (&username[..i], "@", &username[i + 1..]),
        None => (username, "", ""),
    };
    let first = local.chars().next().unwrap_or('?');
    format!("{first}***{at}{domain}")
}
