use std::{sync::Arc, time::{Duration, Instant}};
use anyhow::{Context, Result};
use jsonwebtoken::{decode, decode_header, jwk::JwkSet, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use tokio::sync::RwLock;
use twz_shared::GameUser;

#[derive(Clone)]
pub struct Auth0 {
    issuer: String,
    client_id: String,
    keys: Arc<RwLock<Option<(Instant, JwkSet)>>>,
}

#[derive(Deserialize)]
struct Claims {
    sub: String,
    name: Option<String>,
    email: Option<String>,
}

impl Auth0 {
    #[cfg(test)]
    pub(crate) fn fixture() -> (Self, serde_json::Value) {
        // Generated test-only RSA signatures. The private signing key is discarded.
        let fixtures: serde_json::Value = serde_json::from_str(include_str!("test-tokens.json")).unwrap();
        let keys = serde_json::from_value(fixtures["jwks"].clone()).unwrap();
        (Self {
            issuer: "https://townwarz-test.auth0.com/".into(),
            client_id: "townwarz-test-client".into(),
            keys: Arc::new(RwLock::new(Some((Instant::now(), keys)))),
        }, fixtures)
    }
    pub fn from_env() -> Option<Self> {
        let domain = std::env::var("AUTH0_DOMAIN").ok()?;
        let client_id = std::env::var("AUTH0_CLIENT_ID").ok()?;
        if domain.trim().is_empty() || client_id.trim().is_empty() { return None; }
        Some(Self {
            issuer: format!("https://{}/", domain.trim().trim_end_matches('/')),
            client_id,
            keys: Arc::new(RwLock::new(None)),
        })
    }

    pub async fn verify(&self, http: &reqwest::Client, token: &str) -> Result<GameUser> {
        let header = decode_header(token).context("invalid identity token")?;
        anyhow::ensure!(header.alg == Algorithm::RS256, "Auth0 tokens must use RS256");
        let kid = header.kid.context("missing signing key id")?;
        let cached = self.keys.read().await;
        let key = cached.as_ref().filter(|(fetched, _)| fetched.elapsed() < Duration::from_secs(3600))
            .and_then(|(_, keys)| keys.find(&kid)).cloned();
        drop(cached);
        let key = match key {
            Some(key) => key,
            None => {
                let keys: JwkSet = http.get(format!("{}.well-known/jwks.json", self.issuer))
                    .send().await?.error_for_status()?.json().await?;
                let key = keys.find(&kid).cloned().context("unknown signing key")?;
                *self.keys.write().await = Some((Instant::now(), keys));
                key
            }
        };
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.client_id]);
        validation.validate_nbf = true;
        let claims = decode::<Claims>(token, &DecodingKey::from_jwk(&key)?, &validation)?.claims;
        anyhow::ensure!(!claims.sub.is_empty(), "missing user subject");
        Ok(GameUser { sub: claims.sub, name: claims.name, email: claims.email })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn verifies_identity_and_rejects_expired_wrong_audience_wrong_issuer_and_tampering() {
        let (auth, tokens) = Auth0::fixture();
        let http = reqwest::Client::new();
        let user = auth.verify(&http, tokens["valid"].as_str().unwrap()).await.unwrap();
        assert_eq!(user.sub, "auth0|test-mayor");
        assert_eq!(user.name.as_deref(), Some("Test Mayor"));
        for kind in ["expired", "wrongAudience", "wrongIssuer"] {
            assert!(auth.verify(&http, tokens[kind].as_str().unwrap()).await.is_err(), "{kind}");
        }
        let valid = tokens["valid"].as_str().unwrap();
        let (header_payload, _) = valid.rsplit_once('.').unwrap();
        assert!(auth.verify(&http, &format!("{header_payload}.invalid-signature")).await.is_err());
        assert!(auth.verify(&http, "not-a-token").await.is_err());
    }
}
