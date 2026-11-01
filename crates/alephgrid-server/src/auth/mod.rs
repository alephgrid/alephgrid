//! Identity and authorization (phase 7). See docs/TEAMS.md.
//!
//! * An **organization** has members (`admin` or `member`) and owns **spaces**; a space
//!   holds workbooks (each `workbook.json` names its space).
//! * A space grants roles (`owner`, `editor`, `viewer`) to users and to identity-provider
//!   groups. Organization admins are owners of every space of their organization; the
//!   instance admin (made at setup) is an admin of every organization. Only members of the
//!   organization receive a space's roles, directly or through a group.
//! * A request acts as a [`Principal`]: the local user (`--auth local`, loopback only), a
//!   signed-in user (cookie session) or a service token (`Authorization: Bearer`, one
//!   space, `viewer` or `editor`). Every HTTP route and WebSocket operation names the
//!   [`Cap`] it needs; [`Auth::require_space`] decides.
//! * Objects a principal cannot see are reported as not found, never as forbidden, so an id
//!   from another space reveals nothing.

pub mod oidc;
pub mod routes;
pub mod store;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use axum::http::HeaderMap;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

pub use store::{MemberKind, Org, Space, Store, User};

/// Name of the session cookie.
pub const SESSION_COOKIE: &str = "alephgrid_session";
/// Prefix of service-token secrets.
pub const TOKEN_PREFIX: &str = "agt_";
/// Longest a service token may live.
pub const MAX_TOKEN_DAYS: u64 = 366;
/// Failed sign-ins per email within [`LOCKOUT`] before further attempts are refused.
pub const MAX_FAILURES: u32 = 5;
pub const LOCKOUT: Duration = Duration::from_secs(15 * 60);
pub const MIN_PASSWORD: usize = 10;
/// Headers some proxies set to assert an identity. Alephgrid never trusts them and refuses
/// requests that carry them, so a deployment cannot believe it is protected by them.
pub const IDENTITY_HEADERS: &[&str] =
    &["x-forwarded-user", "x-forwarded-email", "x-forwarded-preferred-username", "x-remote-user", "remote-user", "x-auth-request-user", "x-auth-request-email", "x-webauth-user"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SpaceRole {
    Viewer,
    Editor,
    Owner,
}

impl SpaceRole {
    pub fn as_str(self) -> &'static str {
        match self {
            SpaceRole::Viewer => "viewer",
            SpaceRole::Editor => "editor",
            SpaceRole::Owner => "owner",
        }
    }

    pub fn parse(s: &str) -> Option<SpaceRole> {
        match s {
            "viewer" => Some(SpaceRole::Viewer),
            "editor" => Some(SpaceRole::Editor),
            "owner" => Some(SpaceRole::Owner),
            _ => None,
        }
    }

    /// Whether this role may do `cap`.
    pub fn allows(self, cap: Cap) -> bool {
        self >= cap.least_role()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum OrgRole {
    Member,
    Admin,
}

impl OrgRole {
    pub fn as_str(self) -> &'static str {
        match self {
            OrgRole::Member => "member",
            OrgRole::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Option<OrgRole> {
        match s {
            "member" => Some(OrgRole::Member),
            "admin" => Some(OrgRole::Admin),
            _ => None,
        }
    }
}

/// What a request does to a space or one of its workbooks. Each HTTP route and WebSocket
/// operation names one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Cap {
    /// See the space, open its workbooks and read them.
    View,
    /// Run read-only SQL (still subject to `--disable-sql` and the SQL policy).
    Query,
    /// Download data (table, query and XLSX exports).
    Export,
    /// Change workbook contents: cells, tables, sheets, saved queries and views, imports.
    Edit,
    /// Publish workbook function versions and change their pins.
    Publish,
    /// Create, duplicate, rename, star or move workbooks into this space.
    CreateWorkbook,
    /// Move workbooks to the trash and restore them.
    DeleteWorkbook,
    /// Remove trashed workbooks for good.
    Purge,
    /// Grant and remove roles, create service tokens.
    ManageMembers,
    /// Rename or delete the space.
    ManageSpace,
    /// Choose a workbook's compute profile (7.15); dedicated profiles also need an
    /// organization admin.
    ChooseCompute,
}

impl Cap {
    pub fn least_role(self) -> SpaceRole {
        match self {
            Cap::View | Cap::Query | Cap::Export => SpaceRole::Viewer,
            Cap::Edit | Cap::Publish | Cap::CreateWorkbook | Cap::DeleteWorkbook => SpaceRole::Editor,
            Cap::Purge | Cap::ManageMembers | Cap::ManageSpace | Cap::ChooseCompute => SpaceRole::Owner,
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Cap::View => "view this space",
            Cap::Query => "run SQL in this space",
            Cap::Export => "download data from this space",
            Cap::Edit => "change workbooks in this space",
            Cap::Publish => "publish functions in this space",
            Cap::CreateWorkbook => "add or change workbooks in this space",
            Cap::DeleteWorkbook => "delete workbooks in this space",
            Cap::Purge => "permanently remove workbooks of this space",
            Cap::ManageMembers => "manage this space's members",
            Cap::ManageSpace => "rename or delete this space",
            Cap::ChooseCompute => "choose compute profiles in this space",
        }
    }
}

/// How the server authenticates requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// No sign-in: one local user with every right. Only for a server bound to loopback.
    Local,
    /// Users sign in (password or OIDC); roles apply.
    Accounts,
}

/// Why a request was refused.
#[derive(Debug, thiserror::Error)]
pub enum Denied {
    #[error("sign in to continue")]
    Unauthenticated,
    #[error("you do not have permission to {0}")]
    Forbidden(String),
    /// Refused, with the message as written (sign-in and invitation failures).
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    /// Refused for now: something else is running, or it ran moments ago (HTTP 409).
    #[error("{0}")]
    Conflict(String),
}

impl Denied {
    pub fn code(&self) -> &'static str {
        match self {
            Denied::Unauthenticated => "unauthenticated",
            Denied::Forbidden(_) | Denied::Refused(_) => "forbidden",
            Denied::NotFound(_) => "not_found",
            Denied::BadRequest(_) => "bad_request",
            Denied::Conflict(_) => "conflict",
        }
    }
}

pub fn forbidden(what: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Denied::Forbidden(what.into()))
}

pub fn refused(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Denied::Refused(message.into()))
}

pub fn not_found(what: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Denied::NotFound(what.into()))
}

/// Who is making a request.
#[derive(Debug, Clone)]
pub enum Principal {
    /// `--auth local`: the person at this machine.
    Local,
    User { user: Arc<User>, session: String, expires_at: u64 },
    Token { id: String, name: String, space: String, role: SpaceRole, expires_at: u64 },
}

impl Principal {
    /// How the audit log names this principal.
    pub fn actor(&self) -> String {
        match self {
            Principal::Local => "local".into(),
            Principal::User { user, .. } => format!("user:{}", user.id),
            Principal::Token { id, .. } => format!("token:{id}"),
        }
    }

    pub fn user_id(&self) -> Option<&str> {
        match self {
            Principal::User { user, .. } => Some(&user.id),
            _ => None,
        }
    }

    pub fn is_instance_admin(&self) -> bool {
        match self {
            Principal::Local => true,
            Principal::User { user, .. } => user.instance_admin,
            Principal::Token { .. } => false,
        }
    }

    /// When this principal's credential stops being valid (ms), if ever.
    pub fn expires_at(&self) -> Option<u64> {
        match self {
            Principal::Local => None,
            Principal::User { expires_at, .. } | Principal::Token { expires_at, .. } => Some(*expires_at),
        }
    }
}

/// Settings for sessions and sign-in.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub mode: AuthMode,
    /// Externally visible base URL (`https://grid.example.com`), without a trailing slash.
    /// Required for OIDC; makes cookies `Secure` when it is https, and fixes the origins
    /// and hosts accepted.
    pub public_url: Option<String>,
    pub session_ttl: Duration,
    pub oidc: Option<oidc::OidcConfig>,
    /// Create this instance admin at first start instead of printing a setup code.
    pub bootstrap: Option<(String, String)>,
    /// Host names (without port) requests may address; `None` accepts any. Protects a
    /// loopback server from DNS rebinding.
    pub allowed_hosts: Option<Vec<String>>,
}

/// Names of this machine's loopback interface.
pub fn loopback_hosts() -> Vec<String> {
    vec!["localhost".into(), "127.0.0.1".into(), "[::1]".into()]
}

impl Default for AuthConfig {
    fn default() -> Self {
        AuthConfig {
            mode: AuthMode::Local,
            public_url: None,
            session_ttl: Duration::from_secs(12 * 3600),
            oidc: None,
            bootstrap: None,
            allowed_hosts: Some(loopback_hosts()),
        }
    }
}

pub struct Auth {
    pub config: AuthConfig,
    pub store: Arc<Store>,
    /// Records of workbook changes, imports and exports waiting for the audit writer.
    audit_tx: Mutex<std::sync::mpsc::Sender<AuditMessage>>,
    audit_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// Bumped by every change to identities, roles, sessions, tokens or a workbook's
    /// space; open connections recheck their access when it moves.
    generation: tokio::sync::watch::Sender<u64>,
    failures: Mutex<HashMap<String, (u32, Instant)>>,
    /// One-time code that creates the instance admin while no user exists.
    setup_code: Mutex<Option<String>>,
    pub oidc: Option<oidc::Oidc>,
}

/// 32 random bytes, URL-safe base64.
pub fn secret() -> String {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).expect("the operating system's random source");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// A short random id (16 hex characters).
pub fn random_id() -> String {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).expect("the operating system's random source");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// SHA-256 of a secret, hex. Secrets are stored only this way.
pub fn hash(secret: &str) -> String {
    Sha256::digest(secret.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Constant-time equality of two hashes.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn hash_password(password: &str) -> Result<String> {
    use argon2::password_hash::{PasswordHasher, SaltString};
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|e| anyhow!("random source: {e}"))?;
    let salt = SaltString::encode_b64(&salt).map_err(|e| anyhow!("{e}"))?;
    Ok(argon2::Argon2::default().hash_password(password.as_bytes(), &salt).map_err(|e| anyhow!("{e}"))?.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    PasswordHash::new(hash).is_ok_and(|h| argon2::Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
}

/// A hash to verify against when the email is unknown, so both cases take as long.
fn decoy_hash() -> &'static str {
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| hash_password("decoy password, never matches").unwrap_or_default())
}

pub fn check_password_rules(password: &str) -> Result<()> {
    if password.chars().count() < MIN_PASSWORD {
        bail!(Denied::BadRequest(format!("use a password of at least {MIN_PASSWORD} characters")));
    }
    Ok(())
}

pub fn normalize_email(email: &str) -> Result<String> {
    let e = email.trim().to_lowercase();
    let valid = e.len() <= 254 && e.split_once('@').is_some_and(|(a, d)| !a.is_empty() && d.contains('.') && !d.starts_with('.') && !d.ends_with('.')) && !e.chars().any(char::is_whitespace);
    if !valid {
        bail!(Denied::BadRequest(format!("{email:?} is not an email address")));
    }
    Ok(e)
}

/// The value of cookie `name` in a request.
pub fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get_all(axum::http::header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == name).then_some(v)
    })
}

pub fn now_ms() -> u64 {
    alephgrid_core::storage::now_ms()
}

impl Auth {
    pub fn new(config: AuthConfig, store: Store) -> Result<Auth> {
        let oidc = config.oidc.clone().map(oidc::Oidc::new).transpose()?;
        let store = Arc::new(store);
        let (tx, rx) = std::sync::mpsc::channel();
        let writer = store.clone();
        let thread = std::thread::Builder::new().name("audit".into()).spawn(move || audit_writer(&writer, rx))?;
        let auth = Auth {
            config,
            store,
            audit_tx: Mutex::new(tx),
            audit_thread: Mutex::new(Some(thread)),
            generation: tokio::sync::watch::Sender::new(1),
            failures: Mutex::new(HashMap::new()),
            setup_code: Mutex::new(None),
            oidc,
        };
        if auth.config.mode == AuthMode::Accounts && auth.store.user_count()? == 0 {
            match auth.config.bootstrap.clone() {
                Some((email, password)) => {
                    auth.setup(&email, "Administrator", &password)?;
                    tracing::info!("created the instance admin {email}");
                }
                None => *auth.setup_code.lock().unwrap() = Some(secret()[..12].to_string()),
            }
        }
        Ok(auth)
    }

    pub fn mode(&self) -> AuthMode {
        self.config.mode
    }

    pub fn generation(&self) -> u64 {
        *self.generation.borrow()
    }

    /// Note a change that may alter someone's access.
    pub fn bump(&self) {
        self.generation.send_modify(|g| *g += 1);
    }

    /// Notified at every [`Auth::bump`].
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.generation.subscribe()
    }

    /// The one-time setup code, while the instance has no admin.
    pub fn setup_code(&self) -> Option<String> {
        self.setup_code.lock().unwrap().clone()
    }

    pub fn needs_setup(&self) -> bool {
        self.setup_code.lock().unwrap().is_some()
    }

    /// Create the instance admin: admin of every organization.
    fn setup(&self, email: &str, name: &str, password: &str) -> Result<User> {
        let email = normalize_email(email)?;
        check_password_rules(password)?;
        let hashed = hash_password(password)?;
        let user = self.store.txn(|c| {
            let u = Store::create_user(c, &random_id(), &email, name.trim(), Some(&hashed), None, &[], true)?;
            let mut s = c.prepare("SELECT id FROM orgs")?;
            let orgs: Vec<String> = s.query_map([], |r| r.get(0))?.collect::<duckdb::Result<_>>()?;
            for o in orgs {
                Store::set_org_role(c, &o, &u.id, OrgRole::Admin)?;
            }
            Store::audit_in(c, &store::Audit { actor: "system", action: "instance_admin_created", detail: serde_json::json!({ "user": u.id }), ..Default::default() })?;
            Ok(u)
        })?;
        self.bump();
        Ok(user)
    }

    /// Finish setup with the one-time code.
    pub fn complete_setup(&self, code: &str, email: &str, name: &str, password: &str) -> Result<User> {
        let mut guard = self.setup_code.lock().unwrap();
        match guard.as_deref() {
            Some(c) if same(&hash(c), &hash(code.trim())) => {}
            Some(_) => bail!(Denied::Refused("the setup code is wrong (it is printed when the server starts)".into())),
            None => bail!(Denied::BadRequest("this server is already set up; sign in instead".into())),
        }
        let user = self.setup(email, if name.trim().is_empty() { "Administrator" } else { name }, password)?;
        *guard = None;
        Ok(user)
    }

    // -------------------------------------------------------------- sign-in

    fn locked_out(&self, key: &str) -> Option<Duration> {
        let f = self.failures.lock().unwrap();
        f.get(key).filter(|(n, at)| *n >= MAX_FAILURES && at.elapsed() < LOCKOUT).map(|(_, at)| LOCKOUT - at.elapsed())
    }

    fn failed(&self, key: &str) {
        let mut f = self.failures.lock().unwrap();
        let e = f.entry(key.to_string()).or_insert((0, Instant::now()));
        if e.1.elapsed() >= LOCKOUT {
            *e = (0, Instant::now());
        }
        e.0 += 1;
        e.1 = Instant::now();
    }

    /// Check an email and password; the same message whatever was wrong.
    pub fn password_login(&self, email: &str, password: &str) -> Result<User> {
        let key = email.trim().to_lowercase();
        if let Some(left) = self.locked_out(&key) {
            bail!(Denied::Refused(format!("too many failed sign-ins; try again in {} minute(s)", left.as_secs().div_ceil(60))));
        }
        let user = self.store.user_by_email(&key)?;
        let hash = user.as_ref().and_then(|u| u.password_hash.clone());
        let ok = verify_password(password, hash.as_deref().unwrap_or_else(|| decoy_hash()));
        match user {
            Some(u) if ok && hash.is_some() && !u.disabled => {
                self.failures.lock().unwrap().remove(&key);
                Ok(u)
            }
            _ => {
                self.failed(&key);
                bail!(Denied::Refused("the email or password is wrong".into()))
            }
        }
    }

    /// Start a session for `user`; returns the cookie secret.
    pub fn start_session(&self, user: &User, via: &str) -> Result<(String, u64)> {
        let s = secret();
        let expires = now_ms() + self.config.session_ttl.as_millis() as u64;
        self.store.create_session(&hash(&s), &user.id, expires, via)?;
        Ok((s, expires))
    }

    pub fn end_session(&self, secret: &str) -> Result<()> {
        self.store.delete_session(&hash(secret))?;
        self.bump();
        Ok(())
    }

    /// The `Set-Cookie` value for a session (or clearing it with `None`).
    pub fn session_cookie(&self, value: Option<(&str, u64)>) -> String {
        let secure = self.config.public_url.as_deref().is_some_and(|u| u.starts_with("https://"));
        let attrs = format!("Path=/; HttpOnly; SameSite=Lax{}", if secure { "; Secure" } else { "" });
        match value {
            Some((v, expires)) => format!("{SESSION_COOKIE}={v}; {attrs}; Max-Age={}", expires.saturating_sub(now_ms()) / 1000),
            None => format!("{SESSION_COOKIE}=; {attrs}; Max-Age=0"),
        }
    }

    /// Who sent a request: the local user, a session cookie or a bearer token.
    pub fn authenticate(&self, headers: &HeaderMap) -> Result<Principal> {
        if self.config.mode == AuthMode::Local {
            return Ok(Principal::Local);
        }
        let bearer = headers.get(axum::http::header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "));
        if let Some(b) = bearer {
            return self.token_principal(b.trim());
        }
        match cookie(headers, SESSION_COOKIE) {
            Some(s) if !s.is_empty() => self.session_principal(s),
            _ => bail!(Denied::Unauthenticated),
        }
    }

    fn session_principal(&self, secret: &str) -> Result<Principal> {
        let h = hash(secret);
        let Some(s) = self.store.session(&h)? else { bail!(Denied::Unauthenticated) };
        if s.expires_at <= now_ms() {
            self.store.delete_session(&h)?;
            bail!(Denied::Unauthenticated);
        }
        match self.store.user(&s.user_id)? {
            Some(u) if !u.disabled => Ok(Principal::User { user: Arc::new(u), session: h, expires_at: s.expires_at }),
            _ => bail!(Denied::Unauthenticated),
        }
    }

    fn token_principal(&self, token: &str) -> Result<Principal> {
        let rest = token.strip_prefix(TOKEN_PREFIX).ok_or(Denied::Unauthenticated)?;
        let (id, secret) = rest.split_once('_').ok_or(Denied::Unauthenticated)?;
        let Some((t, stored)) = self.store.token(id)? else { bail!(Denied::Unauthenticated) };
        if !same(&hash(secret), &stored) || t.revoked_at.is_some() || t.expires_at <= now_ms() {
            bail!(Denied::Unauthenticated);
        }
        if self.store.space(&t.space_id)?.is_none() {
            bail!(Denied::Unauthenticated);
        }
        let _ = self.store.token_used(&t.id);
        Ok(Principal::Token { id: t.id, name: t.name, space: t.space_id, role: t.role, expires_at: t.expires_at })
    }

    /// Recheck that a principal's credential is still valid (open connections).
    pub fn still_valid(&self, p: &Principal) -> Result<Principal> {
        match p {
            Principal::Local => Ok(Principal::Local),
            Principal::User { session, .. } => {
                let Some(s) = self.store.session(session)? else { bail!(Denied::Unauthenticated) };
                if s.expires_at <= now_ms() {
                    bail!(Denied::Unauthenticated);
                }
                match self.store.user(&s.user_id)? {
                    Some(u) if !u.disabled => Ok(Principal::User { user: Arc::new(u), session: session.clone(), expires_at: s.expires_at }),
                    _ => bail!(Denied::Unauthenticated),
                }
            }
            Principal::Token { id, .. } => match self.store.token(id)? {
                Some((t, _)) if t.revoked_at.is_none() && t.expires_at > now_ms() && self.store.space(&t.space_id)?.is_some() => {
                    Ok(Principal::Token { id: t.id, name: t.name, space: t.space_id, role: t.role, expires_at: t.expires_at })
                }
                _ => bail!(Denied::Unauthenticated),
            },
        }
    }

    // -------------------------------------------------------- authorization

    /// A principal's role in an organization.
    pub fn org_role(&self, p: &Principal, org: &str) -> Result<Option<OrgRole>> {
        Ok(match p {
            Principal::Local => Some(OrgRole::Admin),
            Principal::User { user, .. } if user.instance_admin => Some(OrgRole::Admin),
            Principal::User { user, .. } => self.store.org_role(org, &user.id)?,
            Principal::Token { .. } => None,
        })
    }

    /// A principal's effective role in a space: the highest of its direct role, its groups'
    /// roles and owner for organization admins. None: no access (not even its existence).
    pub fn space_role(&self, p: &Principal, space: &Space) -> Result<Option<SpaceRole>> {
        match p {
            Principal::Local => Ok(Some(SpaceRole::Owner)),
            Principal::Token { space: s, role, .. } => Ok((s == &space.id).then_some(*role)),
            Principal::User { user, .. } => {
                match self.org_role(p, &space.org_id)? {
                    None => return Ok(None),
                    Some(OrgRole::Admin) => return Ok(Some(SpaceRole::Owner)),
                    Some(OrgRole::Member) => {}
                }
                let best = self
                    .store
                    .space_grants(&space.id)?
                    .into_iter()
                    .filter(|(kind, principal, _)| match kind {
                        MemberKind::User => principal == &user.id,
                        MemberKind::Group => user.groups.iter().any(|g| g == principal),
                    })
                    .map(|(_, _, r)| r)
                    .max();
                Ok(best)
            }
        }
    }

    /// The space, if `p` may do `cap` in it. A space it cannot see is "not found".
    pub fn require_space(&self, p: &Principal, space_id: &str, cap: Cap) -> Result<(Space, SpaceRole)> {
        let space = self.store.space(space_id)?.ok_or_else(|| not_found("space not found"))?;
        match self.space_role(p, &space)? {
            None => Err(not_found("space not found")),
            Some(r) if space.allows(r, cap) => Ok((space, r)),
            Some(_) => Err(forbidden(cap.describe())),
        }
    }

    /// Spaces a principal can see, with its role.
    pub fn visible_spaces(&self, p: &Principal) -> Result<Vec<(Space, SpaceRole)>> {
        let mut out = Vec::new();
        for s in self.store.spaces()? {
            if let Some(r) = self.space_role(p, &s)? {
                out.push((s, r));
            }
        }
        Ok(out)
    }

    /// The organization, if `p` is one of its admins.
    pub fn require_org_admin(&self, p: &Principal, org: &str) -> Result<Org> {
        let o = self.store.org(org)?.ok_or_else(|| not_found("organization not found"))?;
        match self.org_role(p, org)? {
            Some(OrgRole::Admin) => Ok(o),
            Some(OrgRole::Member) => Err(forbidden("manage this organization")),
            None => Err(not_found("organization not found")),
        }
    }

    pub fn require_instance_admin(&self, p: &Principal) -> Result<()> {
        if p.is_instance_admin() {
            Ok(())
        } else {
            Err(forbidden("use server administration"))
        }
    }

    /// Queue a record of what `p` did to a workbook (a change, an import, an export) for the
    /// audit writer, which commits in batches within [`AUDIT_DELAY`]; the change itself does not
    /// wait. A server crash can lose the records of the last fraction of a second; the
    /// workbook's own history (same transaction as each change) still names their actors.
    pub fn record(&self, p: &Principal, action: &str, space: &Space, workbook: &str, detail: serde_json::Value) {
        let r = store::AuditRecord {
            actor: p.actor(),
            action: action.to_string(),
            org_id: Some(space.org_id.clone()),
            space_id: Some(space.id.clone()),
            workbook: Some(workbook.to_string()),
            at_ms: now_ms(),
            detail,
        };
        let _ = self.audit_tx.lock().unwrap().send(AuditMessage::Record(Box::new(r)));
    }

    /// Wait until every record queued so far is written.
    pub fn flush_audit(&self) {
        let (tx, rx) = std::sync::mpsc::channel();
        if self.audit_tx.lock().unwrap().send(AuditMessage::Flush(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_secs(30));
        }
    }

    /// Write what is queued and stop the writer (shutdown).
    pub fn close_audit(&self) {
        self.flush_audit();
        let (tx, _) = std::sync::mpsc::channel();
        drop(std::mem::replace(&mut *self.audit_tx.lock().unwrap(), tx));
        if let Some(t) = self.audit_thread.lock().unwrap().take() {
            let _ = t.join();
        }
    }

    /// Append what `p` did to the audit log; a failure to record is logged, never hidden from
    /// the operator, but does not undo the action that already happened.
    pub fn audit(&self, p: &Principal, action: &str, org: Option<&str>, space: Option<&str>, workbook: Option<&str>, detail: serde_json::Value) {
        let actor = p.actor();
        let a = store::Audit { actor: &actor, action, org_id: org, space_id: space, workbook, denied: false, detail };
        if let Err(e) = self.store.audit(&a) {
            tracing::error!("recording audit event {action}: {e:#}");
        }
    }
}

enum AuditMessage {
    Record(Box<store::AuditRecord>),
    Flush(std::sync::mpsc::Sender<()>),
}

/// Most records waiting before the writer commits them, and the longest wait.
const AUDIT_BATCH: usize = 1000;
const AUDIT_DELAY: Duration = Duration::from_millis(250);

/// Writes audit records in batches until the sender is dropped. A batch that cannot be
/// written is logged with its size and dropped; it never blocks the changes it records.
fn audit_writer(store: &Store, rx: std::sync::mpsc::Receiver<AuditMessage>) {
    use std::sync::mpsc::RecvTimeoutError;
    let mut batch: Vec<store::AuditRecord> = Vec::new();
    let mut waiting: Vec<std::sync::mpsc::Sender<()>> = Vec::new();
    let write = |batch: &mut Vec<store::AuditRecord>, waiting: &mut Vec<std::sync::mpsc::Sender<()>>| {
        if let Err(e) = store.audit_many(batch) {
            tracing::error!("writing {} audit record(s): {e:#}", batch.len());
        }
        batch.clear();
        for w in waiting.drain(..) {
            let _ = w.send(());
        }
    };
    loop {
        let msg = if batch.is_empty() {
            match rx.recv() {
                Ok(m) => Some(m),
                Err(_) => break,
            }
        } else {
            match rx.recv_timeout(AUDIT_DELAY) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        };
        match msg {
            Some(AuditMessage::Record(r)) => {
                batch.push(*r);
                if batch.len() < AUDIT_BATCH {
                    continue;
                }
            }
            Some(AuditMessage::Flush(done)) => waiting.push(done),
            None => {}
        }
        write(&mut batch, &mut waiting);
    }
    write(&mut batch, &mut waiting);
}

/// Requests that try to assert an identity through proxy headers.
pub fn identity_header(headers: &HeaderMap) -> Option<&'static str> {
    IDENTITY_HEADERS.iter().copied().find(|h| headers.contains_key(*h))
}
