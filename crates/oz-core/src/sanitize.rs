//! Sensitive information sanitizer — masks API keys, tokens, and credentials
//! in tool outputs before they reach the LLM context or logs.

/// Patterns that indicate sensitive content.
const SENSITIVE_PATTERNS: &[(&str, &str)] = &[
    ("sk-", "[REDACTED-API-KEY]"),
    ("sk-ant-", "[REDACTED-ANTHROPIC-KEY]"),
    ("AKIA", "[REDACTED-AWS-KEY]"),
    ("eyJ", "[REDACTED-JWT-TOKEN]"),
    ("hf_", "[REDACTED-HF-TOKEN]"),
    (
        "-----BEGIN RSA PRIVATE KEY-----",
        "[REDACTED-PRIVATE-KEY-BLOCK]",
    ),
    (
        "-----BEGIN OPENSSH PRIVATE KEY-----",
        "[REDACTED-SSH-KEY-BLOCK]",
    ),
    (
        "-----BEGIN PGP PRIVATE KEY BLOCK-----",
        "[REDACTED-PGP-KEY-BLOCK]",
    ),
    ("-----END RSA PRIVATE KEY-----", "[REDACTED]"),
    ("-----END OPENSSH PRIVATE KEY-----", "[REDACTED]"),
    ("-----END PGP PRIVATE KEY BLOCK-----", "[REDACTED]"),
    ("ghp_", "[REDACTED-GITHUB-TOKEN]"),
    ("github_pat_", "[REDACTED-GITHUB-PAT]"),
    ("gho_", "[REDACTED-GITHUB-OAUTH]"),
    ("xoxb-", "[REDACTED-SLACK-BOT-TOKEN]"),
    ("xoxp-", "[REDACTED-SLACK-USER-TOKEN]"),
    ("Authorization: Bearer", "[REDACTED-AUTH-HEADER]"),
    ("Authorization: Basic", "[REDACTED-AUTH-HEADER]"),
];

pub fn sanitize(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut i = 0;
    let bytes = content.as_bytes();

    while i < bytes.len() {
        let mut matched = false;
        for (pattern, replacement) in SENSITIVE_PATTERNS {
            let pat_bytes = pattern.as_bytes();
            if i + pat_bytes.len() <= bytes.len() && &bytes[i..i + pat_bytes.len()] == pat_bytes {
                result.push_str(replacement);
                i += pat_bytes.len();
                // Skip rest of the token until whitespace/line-end
                while i < bytes.len() && !is_boundary(bytes[i]) {
                    result.push(if bytes[i] == b'\n' || bytes[i] == b'\r' {
                        bytes[i] as char
                    } else {
                        '*'
                    });
                    i += 1;
                }
                matched = true;
                break;
            }
        }
        if !matched {
            result.push(bytes[i] as char);
            i += 1;
        }
    }

    result
}

fn is_boundary(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'\n' | b'\r' | b'\t' | b',' | b';' | b'"' | b'\'' | b')'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redact_openai_key() {
        let input = "api_key=sk-proj-1234567890abcdef";
        let output = sanitize(input);
        assert!(!output.contains("sk-"));
        assert!(output.contains("[REDACTED-API-KEY]"));
    }

    #[test]
    fn test_redact_aws_key() {
        let input = "AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE";
        let output = sanitize(input);
        assert!(!output.contains("AKIA"));
        assert!(output.contains("[REDACTED-AWS-KEY]"));
    }

    #[test]
    fn test_redact_private_key() {
        let input =
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA...\n-----END RSA PRIVATE KEY-----";
        let output = sanitize(input);
        assert!(
            !output.contains("PRIVATE KEY"),
            "expected PRIVATE KEY to be redacted: {output}"
        );
    }

    #[test]
    fn test_normal_text_passes_through() {
        let input = "Hello world! npm install react";
        let output = sanitize(input);
        assert_eq!(output, input);
    }

    #[test]
    fn test_github_token() {
        let input = "token: ghp_1234567890abcdef1234567890abcdef1234";
        let output = sanitize(input);
        assert!(!output.contains("ghp_"));
    }

    #[test]
    fn test_auth_header() {
        let input = "Authorization: Bearer eyJhbGciOiJIUzI1NiIs...";
        let output = sanitize(input);
        assert!(!output.contains("Bearer"));
        assert!(output.contains("[REDACTED-AUTH-HEADER]"));
    }
}
