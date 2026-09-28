//! Secret generation and hashing. Only SHA-256 hashes of tokens are stored.

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tonic::{Request, Status};

pub const ADMIN_PREFIX: &str = "cmda";
pub const JOIN_PREFIX: &str = "cmdj";
pub const NODE_PREFIX: &str = "cmdn";

pub fn generate_token(prefix: &str) -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("OS random number generator unavailable");
    format!("{prefix}_{}", hex::encode(bytes))
}

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

pub fn hashes_match(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// Hashes of the admin tokens.
pub struct AdminTokens(pub Vec<String>);

impl AdminTokens {
    pub fn accepts(&self, token: &str) -> bool {
        let hash = hash_token(token);
        self.0.iter().any(|admin| hashes_match(admin, &hash))
    }

    /// Interceptor guarding the Control service.
    pub fn check(&self, req: Request<()>) -> Result<Request<()>, Status> {
        let token = req
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(|| Status::unauthenticated("missing admin token"))?;
        if self.accepts(token) {
            Ok(req)
        } else {
            Err(Status::unauthenticated("invalid admin token"))
        }
    }
}
