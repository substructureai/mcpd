use axum::http::{HeaderMap, header::AUTHORIZATION};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("missing credentials")]
    MissingCredentials,
    #[error("invalid credentials")]
    InvalidCredentials,
}

pub trait Authenticator: Send + Sync {
    fn authenticate(&self, headers: &HeaderMap) -> Result<(), AuthError>;
}

pub struct BearerToken {
    hash: [u8; 32],
}

impl BearerToken {
    pub fn new(token: &str) -> Self {
        Self {
            hash: Sha256::digest(token.as_bytes()).into(),
        }
    }
}

impl Authenticator for BearerToken {
    fn authenticate(&self, headers: &HeaderMap) -> Result<(), AuthError> {
        let presented = bearer(headers).ok_or(AuthError::MissingCredentials)?;
        let hash: [u8; 32] = Sha256::digest(presented.as_bytes()).into();
        if hash[..].ct_eq(&self.hash[..]).unwrap_u8() == 1 {
            Ok(())
        } else {
            Err(AuthError::InvalidCredentials)
        }
    }
}

/// Trusts every caller. Only reachable through `--no-auth`, which the daemon
/// announces at startup, because the tools it serves are commands on this
/// machine and nothing else is checking who is asking.
pub struct Anonymous;

impl Authenticator for Anonymous {
    fn authenticate(&self, _headers: &HeaderMap) -> Result<(), AuthError> {
        Ok(())
    }
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then_some(token.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, value.parse().unwrap());
        h
    }

    #[test]
    fn anonymous_accepts_a_caller_with_no_credentials_at_all() {
        assert!(Anonymous.authenticate(&HeaderMap::new()).is_ok());
        assert!(Anonymous.authenticate(&headers("Bearer whatever")).is_ok());
    }

    #[test]
    fn the_configured_token_is_accepted() {
        let auth = BearerToken::new("s3cret");
        assert!(auth.authenticate(&headers("Bearer s3cret")).is_ok());
    }

    #[test]
    fn the_scheme_is_case_insensitive() {
        let auth = BearerToken::new("s3cret");
        assert!(auth.authenticate(&headers("bearer s3cret")).is_ok());
    }

    #[test]
    fn any_other_token_is_rejected() {
        let auth = BearerToken::new("s3cret");
        assert!(matches!(
            auth.authenticate(&headers("Bearer wrong")),
            Err(AuthError::InvalidCredentials)
        ));
    }

    #[test]
    fn a_prefix_of_the_token_is_not_enough() {
        let auth = BearerToken::new("s3cret");
        assert!(auth.authenticate(&headers("Bearer s3cre")).is_err());
    }

    #[test]
    fn another_scheme_is_not_credentials() {
        let auth = BearerToken::new("s3cret");
        assert!(matches!(
            auth.authenticate(&headers("Basic s3cret")),
            Err(AuthError::MissingCredentials)
        ));
    }

    #[test]
    fn no_header_is_missing_rather_than_invalid() {
        let auth = BearerToken::new("s3cret");
        assert!(matches!(
            auth.authenticate(&HeaderMap::new()),
            Err(AuthError::MissingCredentials)
        ));
    }
}
