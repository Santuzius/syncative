//! What a backend's error message says, for the few cases the engine acts on: a sign-in is needed, the provider can't be reached, or rclone was throttled. Messages come from rclone's backends and from Syncative's Proton backend alike; the markers of both are matched on every message.

/// Rate-limit warnings rclone's backends log to stderr while they retry. A line counts when it contains every substring of one inner slice.
pub const RATE_LIMIT_MARKERS: &[&[&str]] = &[
    &["rateLimitExceeded"],
    &["userRateLimitExceeded"],
    &["Quota exceeded"],
];

/// Whether `msg` says the session or token is expired or revoked, so only a new sign-in helps. Not exhaustive: a miss shows the error on the file instead of the sign-in banner.
pub fn is_auth_failure(msg: &str) -> bool {
    const MARKERS: &[&str] = &[
        // HTTP 401 as rclone's OAuth and WebDAV layers and Proton phrase it.
        "code=401",
        "status=401",
        " 401 ",
        "unauthenticated",
        "unauthorized",
        "invalid access token",
        // Google, e.g. "googleapi: Error 401: Request had invalid authentication credentials … Reason: authError".
        "error 401",
        "reason: autherror",
        "invalid authentication credentials",
        // An OAuth refresh token revoked or expired (e.g. a Google app in "testing" mode drops tokens after 7 days).
        "invalid_grant",
        // Proton revoked the session: an invalid refresh token (Code=10013), which go-proton-api wraps as "failed to refresh auth, de-auth: …"; "de-auth" also covers codes the API may add for the same.
        "code=10013",
        "invalid refresh token",
        "session expired",
        "de-auth",
    ];
    if msg.contains(crate::infrastructure::proton::client::REAUTH_HINT) {
        return true;
    }
    let lower = msg.to_ascii_lowercase();
    MARKERS.iter().any(|m| lower.contains(m))
}

/// Whether `msg` says the provider couldn't be reached at all: no network, no DNS, or a connection cut off on the way (e.g. a VPN without a network below it). A single timeout or EOF doesn't count.
pub fn is_offline(msg: &str) -> bool {
    const MARKERS: &[&str] = &[
        "no such host",
        "server misbehaving",
        "temporary failure in name resolution",
        "dial tcp",
        "dial udp",
        "network is unreachable",
        "no route to host",
        "connection refused",
        "connection reset by peer",
        "i/o timeout",
        "tls handshake timeout",
        // go-proton-api's `NetError`.
        "received no response from api",
        "network error while communicating with api",
    ];
    let lower = msg.to_ascii_lowercase();
    MARKERS.iter().any(|m| lower.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::proton::client::REAUTH_HINT;

    #[test]
    fn recognises_unreachable_providers() {
        for msg in [
            r#"received no response from API: Get "https://mail.proton.me/api/drive/shares/x/folders/y/children?Page=0": dial tcp 185.70.42.37:443: connect: connection refused"#,
            "Proton Drive session for 'P' not resumed yet (received no response from API: Get \"https://mail.proton.me/api/core/v4/users\": read tcp 10.111.222.1:48968->185.70.42.37:443: read: connection reset by peer). Retrying automatically — no action needed.",
            r#"couldn't list directory: Get "https://www.googleapis.com/drive/v3/files?alt=json": dial tcp: lookup www.googleapis.com: no such host"#,
        ] {
            assert!(is_offline(msg), "{msg}");
        }
        for msg in [
            "object not found",
            "googleapi: Error 403: Rate Limit Exceeded",
            "unexpected EOF",
        ] {
            assert!(!is_offline(msg), "{msg}");
        }
    }

    /// Resuming a Proton session needs an HTTPS round-trip, so starting before DNS is up (autostart, resume from suspend) must not count as a dead session: that would cost a full sign-in with 2FA for a valid one.
    #[test]
    fn connectivity_failures_are_offline_not_auth() {
        for msg in [
            "received no response from API: Get \"https://mail.proton.me/api/core/v4/users\": dial tcp: lookup mail.proton.me: no such host",
            "network error while communicating with API: connection refused",
            "dial tcp 1.2.3.4:443: connect: network is unreachable",
            "dial tcp 1.2.3.4:443: connect: no route to host",
            "dial tcp: lookup www.googleapis.com: no such host",
        ] {
            assert!(is_offline(msg), "{msg}");
            assert!(!is_auth_failure(msg), "must never ask for a sign-in: {msg}");
        }
    }

    #[test]
    fn revoked_sessions_and_tokens_ask_for_a_sign_in() {
        for msg in [
            "failed to refresh auth, de-auth: Code=10013",
            "Code=401 unauthorized",
            "invalid refresh token",
            "couldn't fetch token: invalid_grant: maybe token expired? - try refreshing with \"rclone config reconnect gdrive:\"",
            "error in ListJSON: couldn't list directory: googleapi: Error 401: Request had invalid authentication credentials. Expected OAuth 2 access token, login cookie or other valid authentication credential.\nMore details:\nReason: authError, Message: Invalid Credentials",
            &format!("Proton Drive session for 'P' not found in keyring. {REAUTH_HINT}"),
        ] {
            assert!(is_auth_failure(msg), "{msg}");
        }
    }

    #[test]
    fn passing_faults_dont_ask_for_a_sign_in() {
        for msg in [
            "Proton Drive session for 'P' not resumed yet (dial tcp: no such host). Retrying automatically — no action needed.",
            // A Secret Service hiccup on the resume path: recoverable, not a dead session.
            "keyring read failed: Platform secure storage failure: Crypto error: Unpad Error",
        ] {
            assert!(!is_auth_failure(msg), "{msg}");
        }
    }
}
