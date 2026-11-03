//! HTTP routes for sign-in, organizations, spaces, memberships, invitations, service tokens
//! and the audit log. Every change is recorded in the audit log and bumps the
//! authorization generation, so open connections recheck their access at once.

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::{json, Value};

use super::store::{Invite, MemberKind, SpaceMember, Store};
use super::{forbidden, not_found, refused, Cap, Denied, OrgRole, Principal, SpaceRole};
use crate::http::{blocking, ApiError, ApiResult};
use crate::state::Shared;

fn bad(msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Denied::BadRequest(msg.into()))
}

fn with_cookie(cookie: String, body: Value) -> Response {
    ([(header::SET_COOKIE, cookie), (header::CACHE_CONTROL, "no-store".to_string())], Json(body)).into_response()
}

/// What `/api/me` says about a principal.
fn describe(state: &Shared, p: &Principal) -> anyhow::Result<Value> {
    let auth = &state.auth;
    Ok(match p {
        Principal::Local => {
            let orgs: Vec<Value> = auth.store.orgs()?.into_iter().map(|o| json!({ "org": o, "role": OrgRole::Admin })).collect();
            json!({ "kind": "local", "instance_admin": true, "orgs": orgs })
        }
        Principal::User { user, expires_at, .. } => {
            let orgs: Vec<Value> = if user.instance_admin {
                auth.store.orgs()?.into_iter().map(|o| json!({ "org": o, "role": OrgRole::Admin })).collect()
            } else {
                auth.store.orgs_of(&user.id)?.into_iter().map(|(o, r)| json!({ "org": o, "role": r })).collect()
            };
            json!({ "kind": "user", "user": user.public(), "instance_admin": user.instance_admin, "orgs": orgs, "expires_at": expires_at })
        }
        Principal::Token { id, name, space, role, expires_at } => {
            json!({ "kind": "token", "token": { "id": id, "name": name, "space_id": space, "role": role, "expires_at": expires_at }, "instance_admin": false, "orgs": [] })
        }
    })
}

pub async fn me(State(state): State<Shared>, Extension(p): Extension<Principal>) -> ApiResult<impl IntoResponse> {
    Ok(Json(blocking(move || describe(&state, &p)).await?))
}

#[derive(Deserialize)]
pub struct SetupBody {
    code: String,
    email: String,
    #[serde(default)]
    name: String,
    password: String,
}

/// Create the instance admin with the setup code printed at startup, and sign in.
pub async fn setup(State(state): State<Shared>, Json(b): Json<SetupBody>) -> ApiResult<Response> {
    blocking(move || {
        let user = state.auth.complete_setup(&b.code, &b.email, &b.name, &b.password)?;
        let (secret, expires) = state.auth.start_session(&user, "password")?;
        let p = Principal::User { user: std::sync::Arc::new(user), session: super::hash(&secret), expires_at: expires };
        state.auth.audit(&p, "signed_in", None, None, None, json!({ "via": "setup" }));
        Ok(with_cookie(state.auth.session_cookie(Some((&secret, expires))), describe(&state, &p)?))
    })
    .await
}

#[derive(Deserialize)]
pub struct LoginBody {
    email: String,
    password: String,
}

pub async fn login(State(state): State<Shared>, Json(b): Json<LoginBody>) -> ApiResult<Response> {
    if state.auth.mode() != super::AuthMode::Accounts {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "this server does not use accounts"));
    }
    blocking(move || {
        let user = match state.auth.password_login(&b.email, &b.password) {
            Ok(u) => u,
            Err(e) => {
                let actor = format!("email:{}", b.email.trim().to_lowercase());
                let _ = state.auth.store.audit(&super::store::Audit { actor: &actor, action: "sign_in_failed", denied: true, detail: json!({ "via": "password" }), ..Default::default() });
                return Err(e);
            }
        };
        let (secret, expires) = state.auth.start_session(&user, "password")?;
        state.auth.store.record_login(&user.id, None)?;
        let p = Principal::User { user: std::sync::Arc::new(user), session: super::hash(&secret), expires_at: expires };
        state.auth.audit(&p, "signed_in", None, None, None, json!({ "via": "password" }));
        Ok(with_cookie(state.auth.session_cookie(Some((&secret, expires))), describe(&state, &p)?))
    })
    .await
}

/// End the session of this browser (connections using it close at once).
pub async fn logout(State(state): State<Shared>, headers: HeaderMap) -> ApiResult<Response> {
    let cookie = super::cookie(&headers, super::SESSION_COOKIE).map(str::to_string);
    let s = state.clone();
    blocking(move || {
        if let Some(c) = cookie.filter(|c| !c.is_empty()) {
            if let Ok(p) = s.auth.authenticate(&headers) {
                s.auth.audit(&p, "signed_out", None, None, None, json!({}));
            }
            s.auth.end_session(&c)?;
        }
        Ok(())
    })
    .await?;
    Ok(([(header::SET_COOKIE, state.auth.session_cookie(None))], StatusCode::NO_CONTENT).into_response())
}

#[derive(Deserialize)]
pub struct PasswordBody {
    current: String,
    new: String,
}

/// Change one's own password; other sessions of the user end.
pub async fn change_password(State(state): State<Shared>, Extension(p): Extension<Principal>, Json(b): Json<PasswordBody>) -> ApiResult<impl IntoResponse> {
    blocking(move || {
        let Principal::User { user, session, .. } = &p else { return Err(bad("only a signed-in user has a password")) };
        let fresh = state.auth.store.user(&user.id)?.ok_or_else(|| anyhow::Error::new(Denied::Unauthenticated))?;
        if !fresh.password_hash.as_deref().is_some_and(|h| super::verify_password(&b.current, h)) {
            return Err(refused("the current password is wrong"));
        }
        super::check_password_rules(&b.new)?;
        state.auth.store.set_password(&user.id, &super::hash_password(&b.new)?)?;
        state.auth.store.txn(|c| {
            c.execute("DELETE FROM sessions WHERE user_id = ? AND hash <> ?", duckdb::params![user.id, session])?;
            Ok(())
        })?;
        state.auth.bump();
        state.auth.audit(&p, "password_changed", None, None, None, json!({}));
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

// ------------------------------------------------------------------ OIDC

#[derive(Deserialize)]
pub struct StartQuery {
    #[serde(default)]
    next: String,
}

pub async fn oidc_start(State(state): State<Shared>, Query(q): Query<StartQuery>) -> ApiResult<Response> {
    let Some(oidc) = state.auth.oidc.as_ref() else { return Err(ApiError::new(StatusCode::NOT_FOUND, "single sign-on is not configured")) };
    let (url, flow) = oidc.begin(&q.next).await.map_err(ApiError::from)?;
    Ok(([(header::SET_COOKIE, oidc.flow_cookie(Some(&flow))), (header::CACHE_CONTROL, "no-store".to_string())], Redirect::to(&url)).into_response())
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    #[serde(default)]
    code: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

fn to_login(message: &str) -> Response {
    let q: String = message.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    Redirect::to(&format!("/login?error={q}")).into_response()
}

/// The identity provider sends the browser back here; sign in and go on. The sign-in's
/// cookie is cleared whatever happens: a state is good for one attempt.
pub async fn oidc_callback(State(state): State<Shared>, Query(q): Query<CallbackQuery>, headers: HeaderMap) -> Response {
    let Some(oidc) = state.auth.oidc.as_ref() else { return to_login("single sign-on is not configured") };
    let mut reply = oidc_finish(&state, oidc, q, &headers).await;
    if let Ok(clear) = header::HeaderValue::from_str(&oidc.flow_cookie(None)) {
        reply.headers_mut().append(header::SET_COOKIE, clear);
    }
    reply
}

async fn oidc_finish(state: &Shared, oidc: &super::oidc::Oidc, q: CallbackQuery, headers: &HeaderMap) -> Response {
    if let Some(e) = q.error {
        return to_login(&format!("the identity provider did not sign you in ({})", q.error_description.unwrap_or(e)));
    }
    let (claims, next) = match oidc.finish(&q.code, &q.state, super::cookie(headers, super::oidc::FLOW_COOKIE)).await {
        Ok(v) => v,
        Err(e) => {
            tracing::info!("single sign-on refused: {e:#}");
            return to_login(&match e.downcast_ref::<Denied>() {
                Some(d) => d.to_string(),
                None => "the identity provider could not be reached; try again".to_string(),
            });
        }
    };
    let s = state.clone();
    let signed = blocking(move || {
        let user = oidc_user(&s, &claims)?;
        let (secret, expires) = s.auth.start_session(&user, "sso")?;
        let p = Principal::User { user: std::sync::Arc::new(user), session: super::hash(&secret), expires_at: expires };
        s.auth.audit(&p, "signed_in", None, None, None, json!({ "via": "sso", "groups": claims.groups.len() }));
        Ok(s.auth.session_cookie(Some((&secret, expires))))
    })
    .await;
    match signed {
        Ok(cookie) => ([(header::SET_COOKIE, cookie)], Redirect::to(&next)).into_response(),
        Err(e) => to_login(&e.1),
    }
}

/// The user an identity-provider sign-in is (7.17): matched by issuer and subject only;
/// a new user needs a verified email and an open invitation for it (or `--oidc-join any`).
/// Groups are taken from every sign-in; invitations for the verified email are applied.
fn oidc_user(state: &Shared, c: &super::oidc::Claims) -> anyhow::Result<super::User> {
    let auth = &state.auth;
    let join = auth.oidc.as_ref().map(|o| o.config.join).unwrap_or(super::oidc::OidcJoin::Invite);
    let verified = c.email.clone().filter(|_| c.email_verified);
    let invites = match &verified {
        Some(e) => auth.store.open_invites_for(e)?,
        None => Vec::new(),
    };
    let default_org = auth.store.setting("default_org")?;
    let user = match auth.store.user_by_oidc(&c.issuer, &c.subject)? {
        Some(u) if u.disabled => return Err(refused("this account is disabled")),
        Some(u) => u,
        None => {
            let Some(email) = verified.clone() else {
                return Err(refused("your identity provider did not confirm an email address for you"));
            };
            if auth.store.user_by_email(&email)?.is_some() {
                return Err(refused(format!(
                    "an account for {email} already exists with a password; sign in with it, or ask an administrator to remove it so single sign-on can be used"
                )));
            }
            if invites.is_empty() && join == super::oidc::OidcJoin::Invite {
                return Err(refused("you have not been invited to this Alephgrid; ask an administrator for an invitation"));
            }
            let name = c.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| email.clone());
            auth.store.txn(|t| Store::create_user(t, &super::random_id(), &email, name.trim(), None, Some((&c.issuer, &c.subject)), &c.groups, false))?
        }
    };
    auth.store.txn(|t| {
        if join == super::oidc::OidcJoin::Any {
            if let Some(org) = &default_org {
                let mut s = t.prepare("SELECT count(*) FROM org_members WHERE org_id = ? AND user_id = ?")?;
                let n: i64 = s.query_row(duckdb::params![org, user.id], |r| r.get(0))?;
                if n == 0 {
                    Store::set_org_role(t, org, &user.id, OrgRole::Member)?;
                }
            }
        }
        if verified.as_deref() == Some(user.email.as_str()) {
            for inv in &invites {
                Store::accept_invite(t, inv, &user.id)?;
            }
        }
        Ok(())
    })?;
    auth.store.record_login(&user.id, Some(&c.groups))?;
    auth.bump();
    Ok(auth.store.user(&user.id)?.unwrap_or(user))
}

// ------------------------------------------------------------- invitations

/// `<id>.<secret>`, as handed out in the invitation link.
fn parse_code(code: &str) -> Option<(&str, &str)> {
    code.trim().split_once('.')
}

fn open_invite(state: &Shared, code: &str) -> anyhow::Result<Invite> {
    let invalid = || not_found("this invitation is not valid; ask for a new one");
    let (id, secret) = parse_code(code).ok_or_else(invalid)?;
    let (inv, stored) = state.auth.store.invite(id)?.ok_or_else(invalid)?;
    if super::hash(secret) != stored {
        return Err(invalid());
    }
    if inv.accepted_at.is_some() || inv.revoked_at.is_some() || inv.expires_at <= super::now_ms() {
        return Err(not_found("this invitation was already used, revoked or has expired; ask for a new one"));
    }
    Ok(inv)
}

#[derive(Deserialize)]
pub struct CodeBody {
    code: String,
}

/// What an invitation is for, to show before accepting it.
pub async fn lookup_invite(State(state): State<Shared>, Json(b): Json<CodeBody>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        blocking(move || {
            let inv = open_invite(&state, &b.code)?;
            let org = state.auth.store.org(&inv.org_id)?.map(|o| o.name);
            let space = match &inv.space_id {
                Some(s) => state.auth.store.space(s)?.map(|s| s.name),
                None => None,
            };
            let existing = state.auth.store.user_by_email(&inv.email)?;
            Ok(json!({
                "email": inv.email,
                "organization": org,
                "space": space,
                "org_role": inv.org_role,
                "space_role": inv.space_role,
                "expires_at": inv.expires_at,
                "existing_user": existing.is_some(),
                "sso": state.auth.oidc.is_some(),
            }))
        })
        .await?,
    ))
}

#[derive(Deserialize)]
pub struct AcceptBody {
    code: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    password: String,
}

/// Accept an invitation: as the signed-in user it names, or by creating that user with a
/// password (signing in).
pub async fn accept_invite(State(state): State<Shared>, headers: HeaderMap, Json(b): Json<AcceptBody>) -> ApiResult<Response> {
    blocking(move || {
        let inv = open_invite(&state, &b.code)?;
        let signed_in = state.auth.authenticate(&headers).ok().filter(|p| matches!(p, Principal::User { .. }));
        let (p, cookie) = match signed_in {
            Some(p) => {
                let Principal::User { user, .. } = &p else { unreachable!() };
                if user.email != inv.email {
                    return Err(refused(format!("this invitation is for {}, and you are signed in as {}", inv.email, user.email)));
                }
                state.auth.store.txn(|c| Store::accept_invite(c, &inv, &user.id))?;
                (p, None)
            }
            None => {
                if state.auth.store.user_by_email(&inv.email)?.is_some() {
                    return Err(bad(format!("sign in as {} first, then open the invitation again", inv.email)));
                }
                super::check_password_rules(&b.password)?;
                let hashed = super::hash_password(&b.password)?;
                let name = if b.name.trim().is_empty() { inv.email.clone() } else { b.name.trim().to_string() };
                let user = state.auth.store.txn(|c| {
                    let u = Store::create_user(c, &super::random_id(), &inv.email, &name, Some(&hashed), None, &[], false)?;
                    Store::accept_invite(c, &inv, &u.id)?;
                    Ok(u)
                })?;
                let (secret, expires) = state.auth.start_session(&user, "password")?;
                let cookie = state.auth.session_cookie(Some((&secret, expires)));
                (Principal::User { user: std::sync::Arc::new(user), session: super::hash(&secret), expires_at: expires }, Some(cookie))
            }
        };
        state.auth.bump();
        state.auth.audit(&p, "invitation_accepted", Some(&inv.org_id), inv.space_id.as_deref(), None, json!({ "invitation": inv.id }));
        let body = describe(&state, &p)?;
        Ok(match cookie {
            Some(c) => with_cookie(c, body),
            None => Json(body).into_response(),
        })
    })
    .await
}

#[derive(Deserialize)]
pub struct InviteBody {
    email: String,
    #[serde(default = "member")]
    org_role: OrgRole,
    #[serde(default)]
    space: Option<String>,
    #[serde(default)]
    space_role: Option<SpaceRole>,
    /// Days until it expires (1 to 30, default 7).
    #[serde(default = "seven")]
    days: u64,
}

fn member() -> OrgRole {
    OrgRole::Member
}

fn seven() -> u64 {
    7
}

pub async fn create_invite(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(org): Path<String>, Json(b): Json<InviteBody>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        blocking(move || {
            state.auth.require_org_admin(&p, &org)?;
            let email = super::normalize_email(&b.email)?;
            if !(1..=30).contains(&b.days) {
                return Err(bad("an invitation lasts 1 to 30 days"));
            }
            let space_role = match (&b.space, b.space_role) {
                (Some(s), r) => {
                    let space = state.auth.store.space(s)?.filter(|sp| sp.org_id == org).ok_or_else(|| not_found("space not found"))?;
                    Some((space.id, r.unwrap_or(SpaceRole::Viewer)))
                }
                (None, Some(_)) => return Err(bad("name the space the role is for")),
                (None, None) => None,
            };
            let secret = super::secret();
            let now = super::now_ms();
            let inv = Invite {
                id: super::random_id(),
                org_id: org.clone(),
                email,
                org_role: b.org_role,
                space_id: space_role.as_ref().map(|s| s.0.clone()),
                space_role: space_role.map(|s| s.1),
                created_by: p.actor(),
                created_at: now,
                expires_at: now + b.days * 86_400_000,
                accepted_at: None,
                revoked_at: None,
            };
            state.auth.store.create_invite(&inv, &super::hash(&secret))?;
            state.auth.audit(&p, "invitation_created", Some(&org), inv.space_id.as_deref(), None, json!({ "invitation": inv.id, "email": inv.email, "org_role": inv.org_role, "space_role": inv.space_role }));
            let code = format!("{}.{secret}", inv.id);
            // The code goes in the fragment: browsers do not send it to servers or in Referer.
            Ok(json!({ "invite": inv, "code": code, "path": format!("/invite#{code}") }))
        })
        .await?,
    ))
}

pub async fn list_invites(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(org): Path<String>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        blocking(move || {
            state.auth.require_org_admin(&p, &org)?;
            state.auth.store.invites_of_org(&org)
        })
        .await?,
    ))
}

pub async fn revoke_invite(State(state): State<Shared>, Extension(p): Extension<Principal>, Path((org, invite)): Path<(String, String)>) -> ApiResult<impl IntoResponse> {
    blocking(move || {
        state.auth.require_org_admin(&p, &org)?;
        let (inv, _) = state.auth.store.invite(&invite)?.filter(|(i, _)| i.org_id == org).ok_or_else(|| not_found("invitation not found"))?;
        state.auth.store.revoke_invite(&inv.id)?;
        state.auth.audit(&p, "invitation_revoked", Some(&org), inv.space_id.as_deref(), None, json!({ "invitation": inv.id }));
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// --------------------------------------------------------- organizations

pub async fn list_orgs(State(state): State<Shared>, Extension(p): Extension<Principal>) -> ApiResult<impl IntoResponse> {
    Ok(Json(blocking(move || Ok(describe(&state, &p)?["orgs"].clone())).await?))
}

#[derive(Deserialize)]
pub struct NameBody {
    name: String,
}

/// Create an organization (instance admin), with a `General` space; its creator is its admin.
pub async fn create_org(State(state): State<Shared>, Extension(p): Extension<Principal>, Json(b): Json<NameBody>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        blocking(move || {
            state.auth.require_instance_admin(&p)?;
            let name = b.name.trim();
            if name.is_empty() {
                return Err(bad("name cannot be empty"));
            }
            let org = state.auth.store.txn(|c| {
                let o = Store::create_org(c, &super::random_id(), name)?;
                if let Some(u) = p.user_id() {
                    Store::set_org_role(c, &o.id, u, OrgRole::Admin)?;
                }
                Store::create_space(c, &super::random_id(), &o.id, "General")?;
                Ok(o)
            })?;
            state.auth.bump();
            state.auth.audit(&p, "organization_created", Some(&org.id), None, None, json!({ "name": org.name }));
            Ok(org)
        })
        .await?,
    ))
}

pub async fn rename_org(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(org): Path<String>, Json(b): Json<NameBody>) -> ApiResult<impl IntoResponse> {
    blocking(move || {
        state.auth.require_org_admin(&p, &org)?;
        if b.name.trim().is_empty() {
            return Err(bad("name cannot be empty"));
        }
        state.auth.store.rename_org(&org, b.name.trim())?;
        state.auth.audit(&p, "organization_renamed", Some(&org), None, None, json!({ "name": b.name.trim() }));
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn org_members(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(org): Path<String>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        blocking(move || {
            state.auth.require_org_admin(&p, &org)?;
            state.auth.store.org_members(&org)
        })
        .await?,
    ))
}

#[derive(Deserialize)]
pub struct RoleBody<R> {
    role: R,
}

pub async fn set_org_role(
    State(state): State<Shared>,
    Extension(p): Extension<Principal>,
    Path((org, user)): Path<(String, String)>,
    Json(b): Json<RoleBody<OrgRole>>,
) -> ApiResult<impl IntoResponse> {
    blocking(move || {
        state.auth.require_org_admin(&p, &org)?;
        state.auth.store.change_org_role(&org, &user, b.role).map_err(|e| bad(format!("{e:#}")))?;
        state.auth.bump();
        state.auth.audit(&p, "organization_role_changed", Some(&org), None, None, json!({ "user": user, "role": b.role }));
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn remove_org_member(State(state): State<Shared>, Extension(p): Extension<Principal>, Path((org, user)): Path<(String, String)>) -> ApiResult<impl IntoResponse> {
    blocking(move || {
        state.auth.require_org_admin(&p, &org)?;
        if state.auth.store.org_role(&org, &user)?.is_none() {
            return Err(not_found("that user is not a member of this organization"));
        }
        state.auth.store.remove_org_member(&org, &user).map_err(|e| bad(format!("{e:#}")))?;
        state.auth.bump();
        state.auth.audit(&p, "organization_member_removed", Some(&org), None, None, json!({ "user": user }));
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct UserPatch {
    #[serde(default)]
    disabled: Option<bool>,
    #[serde(default)]
    name: Option<String>,
}

/// Disable or enable a user (instance admin; ends their sessions), or rename oneself.
pub async fn update_user(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(user): Path<String>, Json(b): Json<UserPatch>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        blocking(move || {
            let me = p.user_id() == Some(user.as_str());
            if b.disabled.is_some() || !me {
                state.auth.require_instance_admin(&p)?;
            }
            let target = state.auth.store.user(&user)?.ok_or_else(|| not_found("user not found"))?;
            if let Some(d) = b.disabled {
                if me && d {
                    return Err(bad("you cannot disable yourself"));
                }
                state.auth.store.set_disabled(&user, d)?;
                state.auth.audit(&p, if d { "user_disabled" } else { "user_enabled" }, None, None, None, json!({ "user": user }));
            }
            if let Some(n) = b.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                state.auth.store.set_user_name(&user, n)?;
            }
            state.auth.bump();
            Ok(state.auth.store.user(&target.id)?.map(|u| u.public()))
        })
        .await?,
    ))
}

#[derive(Deserialize)]
pub struct AuditQuery {
    #[serde(default)]
    before: Option<u64>,
    /// Only events about this workbook.
    #[serde(default)]
    workbook: Option<String>,
    #[serde(default = "hundred")]
    limit: usize,
}

fn hundred() -> usize {
    100
}

pub async fn org_audit(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(org): Path<String>, Query(q): Query<AuditQuery>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        blocking(move || {
            state.auth.require_org_admin(&p, &org)?;
            state.auth.flush_audit();
            state.auth.store.audit_events(Some(&org), q.workbook.as_deref(), q.before, q.limit.clamp(1, 1000))
        })
        .await?,
    ))
}

pub async fn instance_audit(State(state): State<Shared>, Extension(p): Extension<Principal>, Query(q): Query<AuditQuery>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        blocking(move || {
            state.auth.require_instance_admin(&p)?;
            state.auth.flush_audit();
            state.auth.store.audit_events(None, q.workbook.as_deref(), q.before, q.limit.clamp(1, 1000))
        })
        .await?,
    ))
}

// ---------------------------------------------------------------- spaces

/// Spaces the principal can see, with its role and the organization's name.
pub async fn list_spaces(State(state): State<Shared>, Extension(p): Extension<Principal>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        blocking(move || {
            let orgs: std::collections::HashMap<String, String> = state.auth.store.orgs()?.into_iter().map(|o| (o.id, o.name)).collect();
            Ok(state
                .auth
                .visible_spaces(&p)?
                .into_iter()
                .map(|(s, r)| json!({ "id": s.id, "name": s.name, "org_id": s.org_id, "org_name": orgs.get(&s.org_id), "created_at": s.created_at, "role": r, "publish_role": s.publish_role, "python": s.python }))
                .collect::<Vec<_>>())
        })
        .await?,
    ))
}

#[derive(Deserialize)]
pub struct CreateSpaceBody {
    name: String,
    #[serde(default)]
    org: Option<String>,
}

/// Create a space in an organization one belongs to, and become its owner.
pub async fn create_space(State(state): State<Shared>, Extension(p): Extension<Principal>, Json(b): Json<CreateSpaceBody>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        blocking(move || {
            if matches!(p, Principal::Token { .. }) {
                return Err(forbidden("create spaces with a service token"));
            }
            let org = match b.org.filter(|o| !o.is_empty()) {
                Some(o) => o,
                None => {
                    let default = state.auth.store.setting("default_org")?;
                    let mine: Vec<String> = describe(&state, &p)?["orgs"].as_array().into_iter().flatten().filter_map(|o| o["org"]["id"].as_str().map(str::to_string)).collect();
                    match default.filter(|d| mine.contains(d)) {
                        Some(d) => d,
                        None => mine.into_iter().next().ok_or_else(|| forbidden("create spaces: you are not a member of any organization"))?,
                    }
                }
            };
            if state.auth.org_role(&p, &org)?.is_none() {
                return Err(not_found("organization not found"));
            }
            let space = state.create_space(&org, &b.name, p.user_id())?;
            state.auth.audit(&p, "space_created", Some(&org), Some(&space.id), None, json!({ "name": space.name }));
            Ok(space)
        })
        .await?,
    ))
}

#[derive(Deserialize)]
pub struct SpacePatch {
    #[serde(default)]
    name: Option<String>,
    /// The least role that may publish (7.11): `editor` or `owner`.
    #[serde(default)]
    publish_role: Option<SpaceRole>,
    /// Whether Python functions may run (organization admins).
    #[serde(default)]
    python: Option<bool>,
}

/// Rename a space, or change who may publish in it (owners).
pub async fn rename_space(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(id): Path<String>, Json(b): Json<SpacePatch>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        blocking(move || {
            let (space, _) = state.auth.require_space(&p, &id, Cap::ManageSpace)?;
            if let Some(name) = &b.name {
                let renamed = state.auth.store.rename_space(&id, name).map_err(|e| bad(format!("{e:#}")))?;
                state.auth.audit(&p, "space_renamed", Some(&space.org_id), Some(&id), None, json!({ "from": space.name, "to": renamed.name }));
            }
            if let Some(role) = b.publish_role {
                if role == SpaceRole::Viewer {
                    return Err(bad("publishing needs at least an editor"));
                }
                state.auth.store.set_publish_role(&id, role)?;
                state.auth.bump();
                state.auth.audit(&p, "space_publish_role_changed", Some(&space.org_id), Some(&id), None, json!({ "role": role }));
            }
            if let Some(allowed) = b.python {
                state.auth.require_org_admin(&p, &space.org_id)?;
                state.auth.store.set_space_python(&id, allowed)?;
                state.auth.audit(&p, "space_python_changed", Some(&space.org_id), Some(&id), None, json!({ "python": allowed }));
                // Open workbooks of the space stop (or may start) running Python at once.
                state.python_policy_changed(&id);
            }
            state.auth.store.space(&id)?.ok_or_else(|| not_found("space not found"))
        })
        .await?,
    ))
}

pub async fn delete_space(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    blocking(move || {
        let (space, _) = state.auth.require_space(&p, &id, Cap::ManageSpace)?;
        state.delete_space(&id)?;
        state.auth.audit(&p, "space_deleted", Some(&space.org_id), Some(&id), None, json!({ "name": space.name }));
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Direct roles in a space (organization admins are owners of every space besides).
pub async fn space_members(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        blocking(move || {
            state.auth.require_space(&p, &id, Cap::View)?;
            let mut out = Vec::new();
            for (kind, principal, role) in state.auth.store.space_grants(&id)? {
                let user = match kind {
                    MemberKind::User => state.auth.store.user(&principal)?.map(|u| u.public()),
                    MemberKind::Group => None,
                };
                out.push(SpaceMember { kind, principal, role, user });
            }
            Ok(out)
        })
        .await?,
    ))
}

#[derive(Deserialize)]
pub struct MemberBody {
    kind: MemberKind,
    /// A user's id or email, or a group's name.
    principal: String,
    role: SpaceRole,
}

/// Grant a role in a space to a member of its organization or to a group.
pub async fn set_space_member(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(id): Path<String>, Json(b): Json<MemberBody>) -> ApiResult<impl IntoResponse> {
    blocking(move || {
        let (space, _) = state.auth.require_space(&p, &id, Cap::ManageMembers)?;
        let principal = match b.kind {
            MemberKind::User => {
                let who = b.principal.trim();
                let user = if who.contains('@') { state.auth.store.user_by_email(who)? } else { state.auth.store.user(who)? };
                let user = user.ok_or_else(|| bad(format!("there is no user {who}; invite them to the organization first")))?;
                if state.auth.store.org_role(&space.org_id, &user.id)?.is_none() {
                    return Err(bad(format!("{} is not a member of this organization; invite them first", user.email)));
                }
                user.id
            }
            MemberKind::Group => {
                let g = b.principal.trim();
                if g.is_empty() || g.len() > 256 {
                    return Err(bad("name the identity-provider group"));
                }
                g.to_string()
            }
        };
        state.auth.store.txn(|c| Store::set_space_member(c, &id, b.kind, &principal, b.role))?;
        state.auth.bump();
        state.auth.audit(&p, "space_role_granted", Some(&space.org_id), Some(&id), None, json!({ "kind": b.kind, "principal": principal, "role": b.role }));
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn remove_space_member(
    State(state): State<Shared>,
    Extension(p): Extension<Principal>,
    Path((id, kind, principal)): Path<(String, String, String)>,
) -> ApiResult<impl IntoResponse> {
    blocking(move || {
        let (space, _) = state.auth.require_space(&p, &id, Cap::ManageMembers)?;
        let kind = match kind.as_str() {
            "user" => MemberKind::User,
            "group" => MemberKind::Group,
            _ => return Err(bad("kind is user or group")),
        };
        if !state.auth.store.remove_space_member(&id, kind, &principal)? {
            return Err(not_found("no such member"));
        }
        state.auth.bump();
        state.auth.audit(&p, "space_role_removed", Some(&space.org_id), Some(&id), None, json!({ "kind": kind, "principal": principal }));
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// --------------------------------------------------------- service tokens

pub async fn list_tokens(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(id): Path<String>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        blocking(move || {
            state.auth.require_space(&p, &id, Cap::ManageMembers)?;
            state.auth.store.tokens_of_space(&id)
        })
        .await?,
    ))
}

#[derive(Deserialize)]
pub struct TokenBody {
    name: String,
    role: SpaceRole,
    /// Days until it expires (1 to 366).
    days: u64,
}

/// Create a service token for one space (`viewer` or `editor`). Its secret is shown once.
pub async fn create_token(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(id): Path<String>, Json(b): Json<TokenBody>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        blocking(move || {
            let (space, _) = state.auth.require_space(&p, &id, Cap::ManageMembers)?;
            if b.role == SpaceRole::Owner {
                return Err(bad("a service token can be a viewer or an editor, not an owner"));
            }
            if !(1..=super::MAX_TOKEN_DAYS).contains(&b.days) {
                return Err(bad(format!("a token lasts 1 to {} days", super::MAX_TOKEN_DAYS)));
            }
            let name = b.name.trim();
            if name.is_empty() {
                return Err(bad("name the token after what uses it"));
            }
            let (tid, secret) = (super::random_id(), super::secret());
            let token = state.auth.store.create_token(&tid, &super::hash(&secret), name, &id, b.role, &p.actor(), super::now_ms() + b.days * 86_400_000)?;
            state.auth.audit(&p, "token_created", Some(&space.org_id), Some(&id), None, json!({ "token": tid, "name": name, "role": b.role }));
            Ok(json!({ "token": token, "secret": format!("{}{tid}_{secret}", super::TOKEN_PREFIX) }))
        })
        .await?,
    ))
}

pub async fn revoke_token(State(state): State<Shared>, Extension(p): Extension<Principal>, Path((id, token)): Path<(String, String)>) -> ApiResult<impl IntoResponse> {
    blocking(move || {
        let (space, _) = state.auth.require_space(&p, &id, Cap::ManageMembers)?;
        state.auth.store.token(&token)?.filter(|(t, _)| t.space_id == id).ok_or_else(|| not_found("token not found"))?;
        state.auth.store.revoke_token(&token)?;
        state.auth.bump();
        state.auth.audit(&p, "token_revoked", Some(&space.org_id), Some(&id), None, json!({ "token": token }));
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------- quotas (7.07)

/// Usage and limits of every space of an organization (admins).
pub async fn org_usage(State(state): State<Shared>, Extension(p): Extension<Principal>, Path(org): Path<String>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        blocking(move || {
            state.auth.require_org_admin(&p, &org)?;
            state.usage(Some(&org))
        })
        .await?,
    ))
}

pub async fn instance_usage(State(state): State<Shared>, Extension(p): Extension<Principal>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        blocking(move || {
            state.auth.require_instance_admin(&p)?;
            Ok(json!({ "defaults": state.config.quota, "spaces": state.usage(None)? }))
        })
        .await?,
    ))
}

/// `PUT /api/spaces/{id}/profiles`.
#[derive(Debug, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct SpaceProfiles {
    /// Profile ids granted to the space besides `standard`, which always is.
    pub profiles: Vec<String>,
}

/// Grant compute profiles to a space (organization admins, 7.13). Unknown profiles are
/// refused; workbooks of the space that use a profile no longer granted run with
/// `standard` from their next start.
pub async fn set_space_profiles(
    State(state): State<Shared>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
    Json(req): Json<SpaceProfiles>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        blocking(move || {
            let space = state.auth.store.space(&id)?.ok_or_else(|| not_found("space not found"))?;
            if state.auth.space_role(&p, &space)?.is_none() {
                return Err(not_found("space not found"));
            }
            state.auth.require_org_admin(&p, &space.org_id)?;
            let mut profiles: Vec<String> = req.profiles.into_iter().filter(|p| p != crate::runtime::STANDARD).collect();
            profiles.sort();
            profiles.dedup();
            if let Some(unknown) = profiles.iter().find(|p| state.controller.policy.profile(p).is_none()) {
                return Err(anyhow::Error::new(crate::auth::Denied::BadRequest(format!("unknown compute profile {unknown:?}"))));
            }
            state.auth.store.set_space_profiles(&id, &profiles)?;
            state.auth.audit(&p, "space_profiles_changed", Some(&space.org_id), Some(&id), None, json!({ "profiles": profiles }));
            Ok(state.usage(Some(&space.org_id))?.into_iter().find(|u| u.space.id == id))
        })
        .await?,
    ))
}

/// Set a space's own limits (organization admins; owners cannot raise their own). A missing
/// or null field uses the server default.
pub async fn set_space_quota(
    State(state): State<Shared>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
    Json(q): Json<crate::quota::QuotaLimits>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        blocking(move || {
            let space = state.auth.store.space(&id)?.ok_or_else(|| not_found("space not found"))?;
            if state.auth.space_role(&p, &space)?.is_none() {
                return Err(not_found("space not found"));
            }
            state.auth.require_org_admin(&p, &space.org_id)?;
            state.auth.store.set_space_quota(&id, &q)?;
            state.quota.forget(&id);
            state.auth.audit(&p, "space_quota_changed", Some(&space.org_id), Some(&id), None, json!(q));
            Ok(state.usage(Some(&space.org_id))?.into_iter().find(|u| u.space.id == id))
        })
        .await?,
    ))
}
