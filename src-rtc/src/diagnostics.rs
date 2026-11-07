//! Conservative filtering for user-exported support logs.
//!
//! These logs are assembled from many dependencies, so the export policy is
//! deliberately fail-closed for known secret formats and network identifiers.

use std::{net::IpAddr, sync::OnceLock, time::Instant};

const MAX_EXPORTED_LINE_CHARS: usize = 2_000;

static DIAGNOSTICS_STARTED: OnceLock<Instant> = OnceLock::new();

/// Process-local monotonic milliseconds for correlating lifecycle events without
/// depending on wall-clock corrections. Values are diagnostic metadata only.
pub fn monotonic_ms() -> u128 {
    DIAGNOSTICS_STARTED.get_or_init(Instant::now).elapsed().as_millis()
}

/// Return a safe, bounded log line, or `None` when it could expose secret or
/// network configuration material.
pub fn sanitize_diagnostic_log_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || contains_sensitive_material(trimmed) || contains_ip_literal(trimmed) {
        return None;
    }

    let mut output: String = trimmed.chars().take(MAX_EXPORTED_LINE_CHARS).collect();
    if trimmed.chars().count() > MAX_EXPORTED_LINE_CHARS {
        output.push_str("…[truncated]");
    }
    Some(output)
}

fn contains_sensitive_material(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    if [
        "://",
        "stun:",
        "turn:",
        "candidate:",
        "v=0",
        "a=sdp",
        "password",
        "username",
        "private_key",
        "private key",
        "access_token",
        "refresh_token",
        "resume_ticket",
        "session_ticket",
        "session_token",
        "resume_token",
        "authorization",
        "bearer ",
        "pairing_invitation",
        "invitation",
        "invitation_code",
        "invite_code",
        "pair_secret",
        "device_secret",
        "device_grant",
        "registration_secret",
        "signing_key",
        "signing key",
        "signingkey",
        "invite_token",
        "invite token",
        "invitetoken",
        "shared_secret",
        "api_key",
        "registration_key",
        "fingerprint=",
        "host_fingerprint",
        "fingerprint",
        "sdp",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return true;
    }

    // Values in key/value diagnostics are unsafe even when a dependency uses
    // JSON keys or spaces around separators. Plain lifecycle text such as
    // "TURN credentials received" remains useful and contains no value.
    let compact: String = lower
        .chars()
        .filter(|character| !character.is_ascii_whitespace() && !matches!(character, '\"' | '\'' | '_' | '-'))
        .collect();
    [
        "credential=",
        "credential:",
        "credentials=",
        "credentials:",
        "token=",
        "token:",
        "secret=",
        "secret:",
        "sharedsecret=",
        "sharedsecret:",
        "authorization:",
        "ticket=",
        "ticket:",
        "bearer=",
        "bearer:",
        "invite=",
        "invite:",
        "invitetoken=",
        "invitetoken:",
        "invitation=",
        "invitation:",
        "signingkey=",
        "signingkey:",
        "ice_server",
        "turn_uri",
        "stun_uri",
        "iceserver",
        "turnuri",
        "stunuri",
        "candidateaddress",
        "candidateip",
    ]
    .iter()
    .any(|marker| compact.contains(marker))
}

fn contains_ip_literal(line: &str) -> bool {
    // Log lines may be plain text, JSON, or key/value text. Split on
    // everything except characters used by IPv4/IPv6 literals so addresses
    // are still found when surrounded by quotes, labels, CIDR prefixes, or
    // punctuation.
    line.split(|ch: char| !(ch.is_ascii_hexdigit() || matches!(ch, '.' | ':' | '%')))
        .any(contains_ip_token)
}

fn contains_ip_token(token: &str) -> bool {
    let value = token.trim_matches('.');
    if value.is_empty() {
        return false;
    }

    if let Some((bracketed_host, _)) = value.strip_prefix('[').and_then(|v| v.split_once(']')) {
        let without_scope = bracketed_host.split('%').next().unwrap_or(bracketed_host);
        if without_scope.parse::<IpAddr>().is_ok() {
            return true;
        }
    }

    [value, value.trim_end_matches(':')].into_iter().any(|candidate| {
        let without_scope = candidate.split('%').next().unwrap_or(candidate);
        if without_scope.parse::<IpAddr>().is_ok() {
            return true;
        }

        if let Some((address, prefix)) = without_scope.split_once('/')
            && prefix.parse::<u8>().is_ok()
            && address.parse::<IpAddr>().is_ok()
        {
            return true;
        }

        // IPv4 addresses are frequently logged with a port. IPv6 literals
        // remain detectable as the raw address token above.
        without_scope
            .rsplit_once(':')
            .is_some_and(|(host, port)| port.parse::<u16>().is_ok() && host.parse::<IpAddr>().is_ok())
    })
}

#[cfg(test)]
mod tests {
    use super::{monotonic_ms, sanitize_diagnostic_log_line};

    #[test]
    fn monotonic_timestamp_never_moves_backwards() {
        let before = monotonic_ms();
        std::thread::yield_now();
        assert!(monotonic_ms() >= before);
    }

    #[test]
    fn keeps_lifecycle_and_candidate_type_metadata_without_values() {
        assert_eq!(
            sanitize_diagnostic_log_line("[INFO] fresh TURN credentials received valid_for_seconds=600"),
            Some("[INFO] fresh TURN credentials received valid_for_seconds=600".to_owned()),
        );
        assert_eq!(
            sanitize_diagnostic_log_line("[DEBUG] ICE candidate gathered type=relay transport=udp"),
            Some("[DEBUG] ICE candidate gathered type=relay transport=udp".to_owned()),
        );
    }

    #[test]
    fn omits_secrets_sdp_uris_and_raw_candidate_lines() {
        for line in [
            "TURN credentials username=private-user password=private-password",
            "resume_ticket=private-ticket",
            "pairing invitation shockingvrc://pair?token=secret",
            "TURN server turn:relay.example:3478?transport=udp",
            "v=0\r\na=candidate:123 1 udp 1 203.0.113.5 5000 typ host",
            r#"{"credential": "private credential"}"#,
            r#"{"token" : "private token"}"#,
            "TURN credential : private credential",
            "shared secret = private secret",
            "invite_token=private-invitation-token",
            "invite token: private-invitation-token",
            "Authorization: Bearer private-bearer-token",
            "signing_key=private-signing-key",
            "signing-key: private-signing-key",
            "TURN credentials: private-user:private-password",
        ] {
            assert_eq!(sanitize_diagnostic_log_line(line), None, "{line}");
        }
    }

    #[test]
    fn omits_ipv4_and_ipv6_literals_even_outside_candidate_lines() {
        assert_eq!(sanitize_diagnostic_log_line("connected to 192.0.2.15:443"), None);
        assert_eq!(sanitize_diagnostic_log_line("selected remote address [2001:db8::2]:5000"), None);
        assert_eq!(sanitize_diagnostic_log_line(r#"{"remote_addr":"192.0.2.15."}"#), None);
        assert_eq!(sanitize_diagnostic_log_line("peer address 2001:db8::2/64,"), None);
    }

    #[test]
    fn truncates_long_non_sensitive_lines() {
        let output = sanitize_diagnostic_log_line(&"x".repeat(3_000)).unwrap();
        assert!(output.chars().count() < 2_020);
        assert!(output.ends_with("…[truncated]"));
    }
}
