//! OpenID Connect sign-in: authorization code flow with PKCE (S256), `state` and `nonce`,
//! a confidential or public client. The ID token is verified against the issuer's JWKS
//! (asymmetric algorithms only), its issuer, audience, authorized party, expiry and nonce.
//!
//! Identity is the pair (issuer, `sub`); an email is never enough to sign in as an existing
//! user. How claims become users and roles is described in docs/TEAMS.md.
//!
//! A sign-in in progress lives in the browser that started it, not on the server: a short-lived
//! cookie signed with a key of this process holds the state, nonce, PKCE verifier and where to
//! go next. The callback is accepted only from a browser presenting that cookie, so a callback
//! link cannot sign someone else's browser in, and anonymous starts fill no table.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use jsonwebtoken::{jwk::JwkSet, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::Denied;

/// Longest a sign-in may take between leaving for the identity provider and coming back.
pub const PENDING_TTL: Duration = Duration::from_secs(600);
/// The cookie holding a sign-in in progress (see the module comment).
pub const FLOW_COOKIE: &str = "alephgrid_sso";
const MAX_GROUPS: usize = 1000;
/// Discovery documents and keys are fetched again after this long (and on an unknown key).
const CACHE_TTL: Duration = Duration::from_secs(3600);

/// Who may join through the identity provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OidcJoin {
    /// Only people with an open invitation for their verified email, or existing users.
    Invite,
    /// Anyone the identity provider signs in becomes a member of the default organization
    /// (with no space roles until a membership or group mapping grants them).
    Any,
}

#[derive(Debug, Clone)]
pub struct OidcConfig {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub scopes: String,
    /// Claim carrying group names (an array of strings, or one string).
    pub groups_claim: String,
    /// What the sign-in button says.
    pub label: String,
    pub join: OidcJoin,
    /// `<public-url>/api/auth/oidc/callback`.
    pub redirect_uri: String,
}

/// What the identity provider says about the person.
#[derive(Debug, Clone)]
pub struct Claims {
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub name: Option<String>,
    pub groups: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    #[serde(default)]
    token_endpoint_auth_methods_supported: Vec<String>,
}

/// A sign-in in progress, as signed into [`FLOW_COOKIE`].
#[derive(Serialize, Deserialize)]
struct Flow {
    state: String,
    nonce: String,
    verifier: String,
    next: String,
    /// Expiry, seconds since the epoch.
    exp: u64,
}

pub struct Oidc {
    pub config: OidcConfig,
    http: reqwest::Client,
    cache: Mutex<Option<(Instant, Discovery, JwkSet)>>,
    /// Signs [`Flow`] cookies; new on every start, so a restart ends sign-ins in progress.
    flow_key: [u8; 32],
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Whether `url` may be used: https, or http to a loopback address (tests, local IdPs).
fn acceptable(url: &str) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    let Some(rest) = url.strip_prefix("http://") else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit_once(':').map(|(h, p)| if p.chars().all(|c| c.is_ascii_digit()) { h } else { host }).unwrap_or(host);
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

/// Where to go after signing in: only a path on this server.
pub fn safe_next(next: &str) -> String {
    if next.starts_with('/') && !next.starts_with("//") && !next.contains('\\') && !next.chars().any(char::is_control) {
        next.to_string()
    } else {
        "/".to_string()
    }
}

impl Oidc {
    pub fn new(config: OidcConfig) -> Result<Oidc> {
        if !acceptable(&config.issuer) {
            bail!("the OIDC issuer must use https (http only for a loopback address): {}", config.issuer);
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("creating the HTTP client for OIDC")?;
        let mut flow_key = [0u8; 32];
        getrandom::fill(&mut flow_key).map_err(|e| anyhow!("random source: {e}"))?;
        Ok(Oidc { config, http, cache: Mutex::new(None), flow_key })
    }

    async fn fetch_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        if !acceptable(url) {
            bail!("refusing a non-https identity-provider endpoint: {url}");
        }
        let r = self.http.get(url).send().await.with_context(|| format!("contacting the identity provider at {url}"))?;
        if !r.status().is_success() {
            bail!("the identity provider answered {} for {url}", r.status());
        }
        r.json().await.with_context(|| format!("reading {url}"))
    }

    /// Discovery document and keys, cached; `refresh` fetches them again.
    async fn provider(&self, refresh: bool) -> Result<(Discovery, JwkSet)> {
        if !refresh {
            if let Some((at, d, k)) = self.cache.lock().unwrap().as_ref() {
                if at.elapsed() < CACHE_TTL {
                    return Ok((d.clone(), k.clone()));
                }
            }
        }
        let issuer = self.config.issuer.trim_end_matches('/');
        let d: Discovery = self.fetch_json(&format!("{issuer}/.well-known/openid-configuration")).await?;
        if d.issuer != self.config.issuer {
            bail!("the identity provider says its issuer is {:?}, not the configured {:?}", d.issuer, self.config.issuer);
        }
        for u in [&d.authorization_endpoint, &d.token_endpoint, &d.jwks_uri] {
            if !acceptable(u) {
                bail!("refusing a non-https identity-provider endpoint: {u}");
            }
        }
        let keys: JwkSet = self.fetch_json(&d.jwks_uri).await?;
        *self.cache.lock().unwrap() = Some((Instant::now(), d.clone(), keys.clone()));
        Ok((d, keys))
    }

    /// Start a sign-in: the identity provider's URL to send the browser to, and the value of
    /// the [`FLOW_COOKIE`] to give that browser (see [`Oidc::flow_cookie`]).
    pub async fn begin(&self, next: &str) -> Result<(String, String)> {
        let (d, _) = self.provider(false).await?;
        let flow = Flow {
            state: super::secret(),
            nonce: super::secret(),
            verifier: super::secret(),
            next: safe_next(next),
            exp: super::now_ms() / 1000 + PENDING_TTL.as_secs(),
        };
        let challenge = b64(&Sha256::digest(flow.verifier.as_bytes()));
        let cookie = jsonwebtoken::encode(&Header::new(Algorithm::HS256), &flow, &EncodingKey::from_secret(&self.flow_key)).context("signing the sign-in")?;
        let mut url = reqwest::Url::parse(&d.authorization_endpoint).context("the identity provider's authorization endpoint")?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.config.client_id)
            .append_pair("redirect_uri", &self.config.redirect_uri)
            .append_pair("scope", &self.config.scopes)
            .append_pair("state", &flow.state)
            .append_pair("nonce", &flow.nonce)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        Ok((url.to_string(), cookie))
    }

    /// The `Set-Cookie` value for a sign-in in progress (`None` clears it). Scoped to the
    /// sign-in routes; `SameSite=Lax` is sent on the identity provider's redirect back.
    pub fn flow_cookie(&self, value: Option<&str>) -> String {
        let secure = if self.config.redirect_uri.starts_with("https://") { "; Secure" } else { "" };
        match value {
            Some(v) => format!("{FLOW_COOKIE}={v}; Path=/api/auth/oidc; HttpOnly; SameSite=Lax; Max-Age={}{secure}", PENDING_TTL.as_secs()),
            None => format!("{FLOW_COOKIE}=; Path=/api/auth/oidc; HttpOnly; SameSite=Lax; Max-Age=0{secure}"),
        }
    }

    /// Finish a sign-in: check that this browser started it (`flow` is its [`FLOW_COOKIE`]),
    /// exchange the code and verify the ID token. Returns the claims and where to go next.
    pub async fn finish(&self, code: &str, state: &str, flow: Option<&str>) -> Result<(Claims, String)> {
        let again = || anyhow::Error::new(Denied::BadRequest("this sign-in expired, was already used or was started in another browser; start again".into()));
        let mut v = Validation::new(Algorithm::HS256);
        v.leeway = 0;
        v.validate_aud = false;
        v.set_required_spec_claims(&["exp"]);
        let p = flow
            .and_then(|f| jsonwebtoken::decode::<Flow>(f, &DecodingKey::from_secret(&self.flow_key), &v).ok())
            .map(|d| d.claims)
            .filter(|p| super::same(&p.state, state))
            .ok_or_else(again)?;
        let (d, _) = self.provider(false).await?;
        let mut form = vec![
            ("grant_type", "authorization_code".to_string()),
            ("code", code.to_string()),
            ("redirect_uri", self.config.redirect_uri.clone()),
            ("code_verifier", p.verifier.clone()),
        ];
        let mut req = self.http.post(&d.token_endpoint);
        match &self.config.client_secret {
            Some(secret) if d.token_endpoint_auth_methods_supported.iter().any(|m| m == "client_secret_post")
                && !d.token_endpoint_auth_methods_supported.iter().any(|m| m == "client_secret_basic") =>
            {
                form.push(("client_id", self.config.client_id.clone()));
                form.push(("client_secret", secret.clone()));
            }
            Some(secret) => req = req.basic_auth(form_encode(&self.config.client_id), Some(form_encode(secret))),
            None => form.push(("client_id", self.config.client_id.clone())),
        }
        let r = req.form(&form).send().await.context("exchanging the sign-in code with the identity provider")?;
        let status = r.status();
        let body: Value = r.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            let why = body["error"].as_str().unwrap_or("unknown error");
            bail!(Denied::Refused(format!("single sign-on: the identity provider refused the code ({why})")));
        }
        let id_token = body["id_token"].as_str().ok_or_else(|| anyhow!("the identity provider returned no ID token"))?;
        let claims = self.verify(id_token, &p.nonce).await?;
        Ok((claims, p.next))
    }

    /// Verify an ID token's signature and claims.
    pub async fn verify(&self, id_token: &str, nonce: &str) -> Result<Claims> {
        let header = jsonwebtoken::decode_header(id_token).map_err(|e| Denied::Refused(format!("single sign-on: the ID token is malformed ({e})")))?;
        let alg = header.alg;
        if !matches!(alg, Algorithm::RS256 | Algorithm::RS384 | Algorithm::RS512 | Algorithm::PS256 | Algorithm::PS384 | Algorithm::PS512 | Algorithm::ES256 | Algorithm::ES384 | Algorithm::EdDSA) {
            bail!(Denied::Refused(format!("single sign-on: the ID token uses {alg:?}, which is not accepted")));
        }
        let (_, mut keys) = self.provider(false).await?;
        let find = |keys: &JwkSet| -> Option<DecodingKey> {
            let jwk = match &header.kid {
                Some(kid) => keys.find(kid),
                None => keys.keys.first().filter(|_| keys.keys.len() == 1),
            }?;
            DecodingKey::from_jwk(jwk).ok()
        };
        let key = match find(&keys) {
            Some(k) => k,
            None => {
                // The provider may have rotated its keys.
                keys = self.provider(true).await?.1;
                find(&keys).ok_or_else(|| Denied::Refused("single sign-on: the ID token is signed with an unknown key".into()))?
            }
        };
        let mut v = Validation::new(alg);
        v.set_issuer(&[&self.config.issuer]);
        v.set_audience(&[&self.config.client_id]);
        v.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        v.validate_nbf = true;
        v.leeway = 60;
        let data = jsonwebtoken::decode::<Value>(id_token, &key, &v).map_err(|e| Denied::Refused(format!("single sign-on: the ID token is not valid ({e})")))?;
        let c = data.claims;
        if c["nonce"].as_str() != Some(nonce) {
            bail!(Denied::Refused("single sign-on: the ID token does not belong to this sign-in".into()));
        }
        if let Some(aud) = c["aud"].as_array() {
            if aud.len() > 1 && c["azp"].as_str() != Some(self.config.client_id.as_str()) {
                bail!(Denied::Refused("single sign-on: the ID token was issued to another client".into()));
            }
        }
        let subject = c["sub"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| Denied::Refused("single sign-on: the ID token has no subject".into()))?;
        let verified = match &c["email_verified"] {
            Value::Bool(b) => *b,
            Value::String(s) => s == "true",
            _ => false,
        };
        let groups = match &c[self.config.groups_claim.as_str()] {
            Value::Array(a) => a.iter().filter_map(|g| g.as_str()).filter(|g| !g.is_empty() && g.len() <= 256).take(MAX_GROUPS).map(str::to_string).collect(),
            Value::String(s) if !s.is_empty() => vec![s.clone()],
            _ => Vec::new(),
        };
        Ok(Claims {
            issuer: self.config.issuer.clone(),
            subject: subject.to_string(),
            email: c["email"].as_str().map(|e| e.trim().to_lowercase()),
            email_verified: verified,
            name: c["name"].as_str().or(c["preferred_username"].as_str()).map(str::to_string),
            groups,
        })
    }
}

/// `client_secret_basic` encodes id and secret with the form encoding first (RFC 6749 2.3.1).
fn form_encode(s: &str) -> String {
    crate::http::percent_encode(s)
}
