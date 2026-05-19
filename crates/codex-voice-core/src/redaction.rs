/// Redact Bearer tokens in text by replacing the token value with `[redacted]`.
/// Uses an index-based scan so it always advances past each replacement.
pub fn redact_bearer_tokens(text: &str) -> String {
    let mut result = text.to_string();
    let prefix = "Bearer ";
    let mut pos = 0;
    while let Some(found) = result[pos..].find(prefix) {
        let match_start = pos + found;
        let token_start = match_start + prefix.len();
        let token_end = result[token_start..]
            .find(|c: char| c.is_whitespace() || c == '"' || c == ',' || c == '}' || c == '<')
            .map(|i| token_start + i)
            .unwrap_or(result.len());
        if token_end > token_start {
            result.replace_range(token_start..token_end, "[redacted]");
            pos = token_start + "[redacted]".len();
        } else {
            pos = token_start + 1;
        }
    }
    result
}

/// Redact JWTs in text by replacing the full JWT with `[jwt_redacted]`.
/// Uses an index-based scan so it always advances past each match, including malformed ones.
pub fn redact_jwts(text: &str) -> String {
    let mut result = text.to_string();
    let mut pos = 0;
    while let Some(found) = result[pos..].find("eyJ") {
        let start = pos + found;
        if let Some(first_dot) = result[start..].find('.') {
            let first_dot = start + first_dot;
            if let Some(second_dot) = result[first_dot + 1..].find('.') {
                let second_dot = first_dot + 1 + second_dot;
                let end = result[second_dot + 1..]
                    .find(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
                    .map(|i| second_dot + 1 + i)
                    .unwrap_or(result.len());
                result.replace_range(start..end, "[jwt_redacted]");
                pos = start + "[jwt_redacted]".len();
                continue;
            }
        }
        pos = start + 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_single_bearer_token() {
        assert_eq!(
            redact_bearer_tokens("Authorization: Bearer abc123"),
            "Authorization: Bearer [redacted]"
        );
    }

    #[test]
    fn redacts_multiple_bearer_tokens() {
        assert_eq!(
            redact_bearer_tokens("Bearer abc, Bearer def"),
            "Bearer [redacted], Bearer [redacted]"
        );
    }

    #[test]
    fn bearer_redaction_does_not_loop_on_replacement() {
        let input = "Bearer abc";
        let result = redact_bearer_tokens(input);
        // Should complete instantly without infinite loop
        assert_eq!(result, "Bearer [redacted]");
    }

    #[test]
    fn redacts_valid_jwt() {
        let input = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        assert_eq!(redact_jwts(input), "[jwt_redacted]");
    }

    #[test]
    fn jwt_redaction_skips_malformed_eyj_and_finds_later_valid_one() {
        let input = "eyJnot-a-jwt-but eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        assert_eq!(redact_jwts(input), "[jwt_redacted]");
    }

    #[test]
    fn redacts_multiple_jwts() {
        let input = "eyJhbGci.a.b eyJhbGci.c.d";
        assert_eq!(redact_jwts(input), "[jwt_redacted] [jwt_redacted]");
    }
}
