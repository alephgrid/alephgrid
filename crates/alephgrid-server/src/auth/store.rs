//! The control database (`<data-dir>/.control/control.duckdb`): organizations, users,
//! spaces, memberships, sessions, service tokens, invitations and the audit log.
//!
//! It is small and changes rarely, so one connection behind a mutex serves it; every change
//! is one transaction. Secrets (session ids, token and invitation secrets) are stored only
//! as SHA-256 hashes, passwords only as Argon2id hashes. Workbook data never lives here: a
//! workbook names its space in its own `workbook.json`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use alephgrid_core::sql::{ident, lit};
use alephgrid_core::storage::now_ms;
use duckdb::{params, Connection, OptionalExt};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{OrgRole, SpaceRole};

/// Schema version of the control database.
pub const SCHEMA: u32 = 1;

const DDL: &str = "
CREATE TABLE IF NOT EXISTS settings(key VARCHAR PRIMARY KEY, value VARCHAR NOT NULL);
CREATE TABLE IF NOT EXISTS orgs(id VARCHAR PRIMARY KEY, name VARCHAR NOT NULL, created_at BIGINT NOT NULL);
CREATE TABLE IF NOT EXISTS users(
    id VARCHAR PRIMARY KEY, email VARCHAR NOT NULL, name VARCHAR NOT NULL, password_hash VARCHAR,
    oidc_issuer VARCHAR, oidc_subject VARCHAR, groups VARCHAR NOT NULL DEFAULT '[]',
    instance_admin BOOLEAN NOT NULL DEFAULT false, disabled BOOLEAN NOT NULL DEFAULT false,
    created_at BIGINT NOT NULL, last_login BIGINT);
CREATE TABLE IF NOT EXISTS org_members(org_id VARCHAR NOT NULL, user_id VARCHAR NOT NULL, role VARCHAR NOT NULL, PRIMARY KEY(org_id, user_id));
CREATE TABLE IF NOT EXISTS spaces(id VARCHAR PRIMARY KEY, org_id VARCHAR NOT NULL, name VARCHAR NOT NULL, created_at BIGINT NOT NULL, publish_role VARCHAR NOT NULL DEFAULT 'editor');
ALTER TABLE spaces ADD COLUMN IF NOT EXISTS python BOOLEAN DEFAULT true;
CREATE TABLE IF NOT EXISTS library_entries(
    id VARCHAR PRIMARY KEY, space_id VARCHAR NOT NULL, kind VARCHAR NOT NULL, name VARCHAR NOT NULL, created_by VARCHAR NOT NULL,
    created_at BIGINT NOT NULL, retired_at BIGINT);
CREATE TABLE IF NOT EXISTS library_versions(
    entry_id VARCHAR NOT NULL, version BIGINT NOT NULL, payload VARCHAR NOT NULL, source_workbook VARCHAR NOT NULL,
    published_by VARCHAR NOT NULL, published_at BIGINT NOT NULL, PRIMARY KEY(entry_id, version));
CREATE TABLE IF NOT EXISTS space_members(space_id VARCHAR NOT NULL, kind VARCHAR NOT NULL, principal VARCHAR NOT NULL, role VARCHAR NOT NULL, PRIMARY KEY(space_id, kind, principal));
CREATE TABLE IF NOT EXISTS sessions(hash VARCHAR PRIMARY KEY, user_id VARCHAR NOT NULL, created_at BIGINT NOT NULL, expires_at BIGINT NOT NULL, via VARCHAR NOT NULL);
CREATE TABLE IF NOT EXISTS agent_connections(
    id VARCHAR PRIMARY KEY, hash VARCHAR NOT NULL, name VARCHAR NOT NULL, space_id VARCHAR NOT NULL, workbooks VARCHAR,
    created_by VARCHAR NOT NULL, created_at BIGINT NOT NULL, expires_at BIGINT NOT NULL, revoked_at BIGINT, last_used BIGINT);
CREATE TABLE IF NOT EXISTS agent_proposals(
    id VARCHAR PRIMARY KEY, connection_id VARCHAR NOT NULL, operation_id VARCHAR, workbook VARCHAR NOT NULL, kind VARCHAR NOT NULL,
    name VARCHAR NOT NULL, payload VARCHAR NOT NULL, revision BIGINT NOT NULL, created_at BIGINT NOT NULL,
    status VARCHAR NOT NULL, decided_by VARCHAR, decided_at BIGINT, result VARCHAR);
CREATE TABLE IF NOT EXISTS agent_activity(
    connection_id VARCHAR NOT NULL, at_ms BIGINT NOT NULL, tool VARCHAR NOT NULL, workbook VARCHAR, ok BOOLEAN NOT NULL, detail VARCHAR NOT NULL);
CREATE TABLE IF NOT EXISTS tokens(
    id VARCHAR PRIMARY KEY, hash VARCHAR NOT NULL, name VARCHAR NOT NULL, space_id VARCHAR NOT NULL, role VARCHAR NOT NULL,
    created_by VARCHAR NOT NULL, created_at BIGINT NOT NULL, expires_at BIGINT NOT NULL, revoked_at BIGINT, last_used BIGINT);
CREATE TABLE IF NOT EXISTS invites(
    id VARCHAR PRIMARY KEY, hash VARCHAR NOT NULL, org_id VARCHAR NOT NULL, email VARCHAR NOT NULL, org_role VARCHAR NOT NULL,
    space_id VARCHAR, space_role VARCHAR, created_by VARCHAR NOT NULL, created_at BIGINT NOT NULL, expires_at BIGINT NOT NULL,
    accepted_by VARCHAR, accepted_at BIGINT, revoked_at BIGINT);
CREATE TABLE IF NOT EXISTS space_quotas(space_id VARCHAR PRIMARY KEY, storage_bytes BIGINT, workbooks BIGINT, jobs BIGINT, open_workbooks BIGINT);
ALTER TABLE space_quotas ADD COLUMN IF NOT EXISTS compute_memory_bytes BIGINT;
CREATE TABLE IF NOT EXISTS space_profiles(space_id VARCHAR NOT NULL, profile VARCHAR NOT NULL, PRIMARY KEY(space_id, profile));
CREATE TABLE IF NOT EXISTS audit(
    seq BIGINT PRIMARY KEY, at_ms BIGINT NOT NULL, actor VARCHAR NOT NULL, action VARCHAR NOT NULL, org_id VARCHAR,
    space_id VARCHAR, workbook VARCHAR, outcome VARCHAR NOT NULL, detail VARCHAR NOT NULL);
";

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct Org {
    pub id: String,
    pub name: String,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct Space {
    pub id: String,
    pub org_id: String,
    pub name: String,
    pub created_at: u64,
    /// The least role that may publish: function versions and pins in the space's
    /// workbooks, and entries of its library (7.11). `editor` or `owner`.
    pub publish_role: SpaceRole,
    /// Whether Python functions may run for its workbooks (phase 8); organization admins
    /// turn it off.
    pub python: bool,
}

impl Space {
    /// Whether `role` may do `cap` here: publication follows the space's own setting.
    pub fn allows(&self, role: SpaceRole, cap: super::Cap) -> bool {
        match cap {
            super::Cap::Publish => role >= self.publish_role.max(SpaceRole::Editor),
            _ => role.allows(cap),
        }
    }
}

fn space_row(r: &duckdb::Row) -> duckdb::Result<Space> {
    let publish: String = r.get(4)?;
    Ok(Space {
        id: r.get(0)?,
        org_id: r.get(1)?,
        name: r.get(2)?,
        created_at: r.get::<_, i64>(3)? as u64,
        publish_role: SpaceRole::parse(&publish).unwrap_or(SpaceRole::Editor),
        python: r.get::<_, Option<bool>>(5)?.unwrap_or(true),
    })
}

/// A user as stored; [`User::public`] is what clients see.
#[derive(Debug, Clone)]
pub struct User {
    pub id: String,
    pub email: String,
    pub name: String,
    pub password_hash: Option<String>,
    pub oidc_issuer: Option<String>,
    pub oidc_subject: Option<String>,
    /// Identity-provider groups from the last sign-in.
    pub groups: Vec<String>,
    pub instance_admin: bool,
    pub disabled: bool,
    pub created_at: u64,
    pub last_login: Option<u64>,
}

/// What clients may see of a user (never hashes or identity-provider subjects).
#[derive(Debug, Clone, Serialize, TS)]
pub struct PublicUser {
    pub id: String,
    pub email: String,
    pub name: String,
    pub instance_admin: bool,
    pub disabled: bool,
    /// `password`, `sso` or both.
    pub sign_in: Vec<String>,
    pub groups: Vec<String>,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_login: Option<u64>,
}

impl User {
    pub fn public(&self) -> PublicUser {
        let mut sign_in = Vec::new();
        if self.password_hash.is_some() {
            sign_in.push("password".to_string());
        }
        if self.oidc_subject.is_some() {
            sign_in.push("sso".to_string());
        }
        PublicUser {
            id: self.id.clone(),
            email: self.email.clone(),
            name: self.name.clone(),
            instance_admin: self.instance_admin,
            disabled: self.disabled,
            sign_in,
            groups: self.groups.clone(),
            last_login: self.last_login,
        }
    }
}

/// A member of an organization.
#[derive(Debug, Clone, Serialize, TS)]
pub struct OrgMember {
    pub user: PublicUser,
    pub role: OrgRole,
}

/// Who a space membership is for: a user (by id) or an identity-provider group (by name).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MemberKind {
    User,
    Group,
}

impl MemberKind {
    fn as_str(self) -> &'static str {
        match self {
            MemberKind::User => "user",
            MemberKind::Group => "group",
        }
    }

    fn parse(s: &str) -> MemberKind {
        if s == "group" { MemberKind::Group } else { MemberKind::User }
    }
}

#[derive(Debug, Clone, Serialize, TS)]
pub struct SpaceMember {
    pub kind: MemberKind,
    /// User id or group name.
    pub principal: String,
    pub role: SpaceRole,
    /// For a user: their email and name.
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<PublicUser>,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub user_id: String,
    pub created_at: u64,
    pub expires_at: u64,
}

/// A service token as listed (never its secret).
#[derive(Debug, Clone, Serialize, TS)]
pub struct Token {
    pub id: String,
    pub name: String,
    pub space_id: String,
    pub role: SpaceRole,
    pub created_by: String,
    pub created_at: u64,
    pub expires_at: u64,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<u64>,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used: Option<u64>,
}

#[derive(Debug, Clone, Serialize, TS)]
pub struct Invite {
    pub id: String,
    pub org_id: String,
    pub email: String,
    pub org_role: OrgRole,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space_role: Option<SpaceRole>,
    pub created_by: String,
    pub created_at: u64,
    pub expires_at: u64,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted_at: Option<u64>,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, TS)]
pub struct AuditEvent {
    pub seq: u64,
    pub at: u64,
    /// `user:<id>`, `token:<id>`, `local` or `system`.
    pub actor: String,
    pub action: String,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workbook: Option<String>,
    /// `ok` or `denied`.
    pub outcome: String,
    #[ts(type = "Record<string, unknown>")]
    pub detail: serde_json::Value,
}

/// An audit record to append.
#[derive(Debug, Clone, Default)]
pub struct Audit<'a> {
    pub actor: &'a str,
    pub action: &'a str,
    pub org_id: Option<&'a str>,
    pub space_id: Option<&'a str>,
    pub workbook: Option<&'a str>,
    pub denied: bool,
    pub detail: serde_json::Value,
}

/// An audit record owned for writing later (see [`super::Auth::record`]).
#[derive(Debug, Clone, Default)]
pub struct AuditRecord {
    pub actor: String,
    pub action: String,
    pub org_id: Option<String>,
    pub space_id: Option<String>,
    pub workbook: Option<String>,
    pub at_ms: u64,
    pub detail: serde_json::Value,
}

pub struct Store {
    conn: Mutex<Connection>,
}

fn user_row(r: &duckdb::Row) -> duckdb::Result<User> {
    let groups: String = r.get(6)?;
    Ok(User {
        id: r.get(0)?,
        email: r.get(1)?,
        name: r.get(2)?,
        password_hash: r.get(3)?,
        oidc_issuer: r.get(4)?,
        oidc_subject: r.get(5)?,
        groups: serde_json::from_str(&groups).unwrap_or_default(),
        instance_admin: r.get(7)?,
        disabled: r.get(8)?,
        created_at: r.get::<_, i64>(9)? as u64,
        last_login: r.get::<_, Option<i64>>(10)?.map(|v| v as u64),
    })
}

const USER_COLS: &str = "id, email, name, password_hash, oidc_issuer, oidc_subject, groups, instance_admin, disabled, created_at, last_login";

fn token_row(r: &duckdb::Row) -> duckdb::Result<Token> {
    Ok(Token {
        id: r.get(0)?,
        name: r.get(1)?,
        space_id: r.get(2)?,
        role: SpaceRole::parse(&r.get::<_, String>(3)?).unwrap_or(SpaceRole::Viewer),
        created_by: r.get(4)?,
        created_at: r.get::<_, i64>(5)? as u64,
        expires_at: r.get::<_, i64>(6)? as u64,
        revoked_at: r.get::<_, Option<i64>>(7)?.map(|v| v as u64),
        last_used: r.get::<_, Option<i64>>(8)?.map(|v| v as u64),
    })
}

const TOKEN_COLS: &str = "id, name, space_id, role, created_by, created_at, expires_at, revoked_at, last_used";

fn invite_row(r: &duckdb::Row) -> duckdb::Result<Invite> {
    Ok(Invite {
        id: r.get(0)?,
        org_id: r.get(1)?,
        email: r.get(2)?,
        org_role: OrgRole::parse(&r.get::<_, String>(3)?).unwrap_or(OrgRole::Member),
        space_id: r.get(4)?,
        space_role: r.get::<_, Option<String>>(5)?.and_then(|s| SpaceRole::parse(&s)),
        created_by: r.get(6)?,
        created_at: r.get::<_, i64>(7)? as u64,
        expires_at: r.get::<_, i64>(8)? as u64,
        accepted_at: r.get::<_, Option<i64>>(9)?.map(|v| v as u64),
        revoked_at: r.get::<_, Option<i64>>(10)?.map(|v| v as u64),
    })
}

const INVITE_COLS: &str = "id, org_id, email, org_role, space_id, space_role, created_by, created_at, expires_at, accepted_at, revoked_at";

impl Store {
    /// Open the control database at `<data dir>/.control/control.duckdb`.
    pub fn open(path: &Path) -> Result<Store> {
        Store::open_allowing(path, &[])
    }

    /// [`Store::open`], also allowing backups into `extra` directories. A control database
    /// may come from a backup archive someone else made, so, like a workbook, it is opened
    /// with the engine locked down before anything in it is read: no extensions, and files
    /// only in its own directory and the data directory's `.backups`.
    pub fn open_allowing(path: &Path, extra: &[PathBuf]) -> Result<Store> {
        let dir = path.parent().unwrap_or(Path::new("."));
        let mut allowed = vec![dir.to_path_buf()];
        if let Some(data_dir) = dir.parent() {
            allowed.push(data_dir.join(".backups"));
        }
        allowed.extend(extra.iter().cloned());
        let lock = alephgrid_core::storage::Lockdown { allowed, settings: Vec::new() };
        let conn = alephgrid_core::storage::connect_locked(path, &lock).with_context(|| format!("opening the control database {}", path.display()))?;
        conn.execute_batch("SET memory_limit = '128MB'; SET threads = 1;")?;
        conn.execute_batch(DDL)?;
        let version: Option<String> = conn.query_row("SELECT value FROM settings WHERE key = 'schema'", [], |r| r.get(0)).optional()?;
        match version.as_deref().map(str::parse::<u32>) {
            None => {
                conn.execute("INSERT INTO settings VALUES ('schema', ?)", params![SCHEMA.to_string()])?;
            }
            Some(Ok(v)) if v == SCHEMA => {}
            Some(Ok(v)) if v > SCHEMA => bail!("the control database has schema {v}, newer than this server understands ({SCHEMA}); upgrade Alephgrid"),
            Some(v) => bail!("unexpected control database schema {v:?}"),
        }
        // No user SQL ever runs here; queries are fixed statements with bound parameters.
        Ok(Store { conn: Mutex::new(conn) })
    }

    /// Run `f` in one transaction.
    pub fn txn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch("BEGIN TRANSACTION;")?;
        match f(&conn) {
            Ok(v) => match conn.execute_batch("COMMIT;") {
                Ok(()) => Ok(v),
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK;");
                    Err(e.into())
                }
            },
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        f(&self.conn.lock().unwrap())
    }

    /// A consistent copy of the whole database into a new file (backups).
    pub fn backup_to(&self, path: &Path) -> Result<()> {
        if path.exists() {
            bail!("{} already exists", path.display());
        }
        let conn = self.conn.lock().unwrap();
        let target = lit(&path.to_string_lossy());
        let db: String = conn.query_row("SELECT current_database()", [], |r| r.get(0))?;
        let copy = (|| -> Result<()> {
            conn.execute_batch(&format!("ATTACH {target} AS ag_backup;"))?;
            conn.execute_batch("BEGIN TRANSACTION;")?;
            let r = conn.execute_batch(&format!("COPY FROM DATABASE {} TO ag_backup;", ident(&db)));
            match r {
                Ok(()) => {
                    if let Err(e) = conn.execute_batch("COMMIT;") {
                        let _ = conn.execute_batch("ROLLBACK;");
                        return Err(e.into());
                    }
                }
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK;");
                    return Err(e.into());
                }
            }
            Ok(())
        })();
        let _ = conn.execute_batch("DETACH ag_backup;");
        copy
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        self.read(|c| Ok(c.query_row("SELECT value FROM settings WHERE key = ?", params![key], |r| r.get(0)).optional()?))
    }

    pub fn set_setting(c: &Connection, key: &str, value: &str) -> Result<()> {
        c.execute("INSERT INTO settings VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value", params![key, value])?;
        Ok(())
    }

    // ------------------------------------------------------------------ orgs

    pub fn create_org(c: &Connection, id: &str, name: &str) -> Result<Org> {
        let org = Org { id: id.to_string(), name: name.to_string(), created_at: now_ms() };
        c.execute("INSERT INTO orgs VALUES (?, ?, ?)", params![org.id, org.name, org.created_at as i64])?;
        Ok(org)
    }

    pub fn orgs(&self) -> Result<Vec<Org>> {
        self.read(|c| {
            let mut s = c.prepare("SELECT id, name, created_at FROM orgs ORDER BY created_at, id")?;
            let rows = s.query_map([], |r| Ok(Org { id: r.get(0)?, name: r.get(1)?, created_at: r.get::<_, i64>(2)? as u64 }))?;
            Ok(rows.collect::<duckdb::Result<Vec<_>>>()?)
        })
    }

    pub fn org(&self, id: &str) -> Result<Option<Org>> {
        Ok(self.orgs()?.into_iter().find(|o| o.id == id))
    }

    pub fn rename_org(&self, id: &str, name: &str) -> Result<()> {
        self.txn(|c| {
            if c.execute("UPDATE orgs SET name = ? WHERE id = ?", params![name, id])? == 0 {
                bail!("organization not found");
            }
            Ok(())
        })
    }

    // ----------------------------------------------------------------- users

    pub fn user(&self, id: &str) -> Result<Option<User>> {
        self.read(|c| Ok(c.query_row(&format!("SELECT {USER_COLS} FROM users WHERE id = ?"), params![id], user_row).optional()?))
    }

    pub fn user_by_email(&self, email: &str) -> Result<Option<User>> {
        self.read(|c| Self::user_by_email_in(c, email))
    }

    pub fn user_by_email_in(c: &Connection, email: &str) -> Result<Option<User>> {
        Ok(c.query_row(&format!("SELECT {USER_COLS} FROM users WHERE email = ?"), params![email.to_lowercase()], user_row).optional()?)
    }

    pub fn user_by_oidc(&self, issuer: &str, subject: &str) -> Result<Option<User>> {
        self.read(|c| {
            Ok(c.query_row(&format!("SELECT {USER_COLS} FROM users WHERE oidc_issuer = ? AND oidc_subject = ?"), params![issuer, subject], user_row)
                .optional()?)
        })
    }

    pub fn user_count(&self) -> Result<u64> {
        self.read(|c| Ok(c.query_row("SELECT count(*) FROM users", [], |r| r.get::<_, i64>(0))? as u64))
    }

    /// Create a user. Emails are unique (case-insensitively); so are identity-provider
    /// subjects per issuer.
    #[allow(clippy::too_many_arguments)]
    pub fn create_user(
        c: &Connection,
        id: &str,
        email: &str,
        name: &str,
        password_hash: Option<&str>,
        oidc: Option<(&str, &str)>,
        groups: &[String],
        instance_admin: bool,
    ) -> Result<User> {
        let email = email.trim().to_lowercase();
        if Self::user_by_email_in(c, &email)?.is_some() {
            bail!("a user with the email {email} already exists");
        }
        if let Some((iss, sub)) = oidc {
            let n: i64 = c.query_row("SELECT count(*) FROM users WHERE oidc_issuer = ? AND oidc_subject = ?", params![iss, sub], |r| r.get(0))?;
            if n > 0 {
                bail!("this identity-provider account is already linked to a user");
            }
        }
        let t = now_ms();
        c.execute(
            "INSERT INTO users (id, email, name, password_hash, oidc_issuer, oidc_subject, groups, instance_admin, disabled, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, false, ?)",
            params![id, email, name, password_hash, oidc.map(|o| o.0), oidc.map(|o| o.1), serde_json::to_string(groups)?, instance_admin, t as i64],
        )?;
        Ok(User {
            id: id.to_string(),
            email,
            name: name.to_string(),
            password_hash: password_hash.map(str::to_string),
            oidc_issuer: oidc.map(|o| o.0.to_string()),
            oidc_subject: oidc.map(|o| o.1.to_string()),
            groups: groups.to_vec(),
            instance_admin,
            disabled: false,
            created_at: t,
            last_login: None,
        })
    }

    /// Record a sign-in: time, and for an identity-provider sign-in the groups it carried.
    pub fn record_login(&self, user: &str, groups: Option<&[String]>) -> Result<()> {
        self.txn(|c| {
            c.execute("UPDATE users SET last_login = ? WHERE id = ?", params![now_ms() as i64, user])?;
            if let Some(g) = groups {
                c.execute("UPDATE users SET groups = ? WHERE id = ?", params![serde_json::to_string(g)?, user])?;
            }
            Ok(())
        })
    }

    pub fn set_password(&self, user: &str, hash: &str) -> Result<()> {
        self.txn(|c| {
            c.execute("UPDATE users SET password_hash = ? WHERE id = ?", params![hash, user])?;
            Ok(())
        })
    }

    pub fn set_user_name(&self, user: &str, name: &str) -> Result<()> {
        self.txn(|c| {
            c.execute("UPDATE users SET name = ? WHERE id = ?", params![name, user])?;
            Ok(())
        })
    }

    /// Disable or re-enable a user; disabling also ends their sessions.
    pub fn set_disabled(&self, user: &str, disabled: bool) -> Result<()> {
        self.txn(|c| {
            if c.execute("UPDATE users SET disabled = ? WHERE id = ?", params![disabled, user])? == 0 {
                bail!("user not found");
            }
            if disabled {
                c.execute("DELETE FROM sessions WHERE user_id = ?", params![user])?;
            }
            Ok(())
        })
    }

    // ----------------------------------------------------------- org members

    pub fn org_role(&self, org: &str, user: &str) -> Result<Option<OrgRole>> {
        self.read(|c| Self::org_role_in(c, org, user))
    }

    fn org_role_in(c: &Connection, org: &str, user: &str) -> Result<Option<OrgRole>> {
        let r: Option<String> = c.query_row("SELECT role FROM org_members WHERE org_id = ? AND user_id = ?", params![org, user], |r| r.get(0)).optional()?;
        Ok(r.and_then(|r| OrgRole::parse(&r)))
    }

    /// Whether `user` has any row in `org`'s members.
    pub fn is_org_member(c: &Connection, org: &str, user: &str) -> Result<bool> {
        let n: i64 = c.query_row("SELECT count(*) FROM org_members WHERE org_id = ? AND user_id = ?", params![org, user], |r| r.get(0))?;
        Ok(n > 0)
    }

    pub fn set_org_role(c: &Connection, org: &str, user: &str, role: OrgRole) -> Result<()> {
        c.execute(
            "INSERT INTO org_members VALUES (?, ?, ?) ON CONFLICT (org_id, user_id) DO UPDATE SET role = excluded.role",
            params![org, user, role.as_str()],
        )?;
        Ok(())
    }

    /// Change a member's role. The last admin of an organization cannot be demoted.
    pub fn change_org_role(&self, org: &str, user: &str, role: OrgRole) -> Result<()> {
        self.txn(|c| {
            if Self::org_role_in(c, org, user)?.is_none() {
                bail!("that user is not a member of this organization");
            }
            if role != OrgRole::Admin {
                Self::keep_an_admin(c, org, user)?;
            }
            Self::set_org_role(c, org, user, role)
        })
    }

    fn keep_an_admin(c: &Connection, org: &str, leaving: &str) -> Result<()> {
        let others: i64 = c.query_row(
            "SELECT count(*) FROM org_members m JOIN users u ON u.id = m.user_id WHERE m.org_id = ? AND m.role = 'admin' AND m.user_id <> ? AND NOT u.disabled",
            params![org, leaving],
            |r| r.get(0),
        )?;
        let is_admin = Self::org_role_in(c, org, leaving)? == Some(OrgRole::Admin);
        if is_admin && others == 0 {
            bail!("an organization needs at least one admin; make someone else an admin first");
        }
        Ok(())
    }

    /// Remove a user from an organization, with their memberships of its spaces.
    pub fn remove_org_member(&self, org: &str, user: &str) -> Result<()> {
        self.txn(|c| {
            Self::keep_an_admin(c, org, user)?;
            c.execute("DELETE FROM org_members WHERE org_id = ? AND user_id = ?", params![org, user])?;
            c.execute(
                "DELETE FROM space_members WHERE kind = 'user' AND principal = ? AND space_id IN (SELECT id FROM spaces WHERE org_id = ?)",
                params![user, org],
            )?;
            Ok(())
        })
    }

    pub fn org_members(&self, org: &str) -> Result<Vec<OrgMember>> {
        self.read(|c| {
            let cols = USER_COLS.split(", ").map(|c| format!("u.{c}")).collect::<Vec<_>>().join(", ");
            let mut s = c.prepare(&format!("SELECT {cols}, m.role FROM org_members m JOIN users u ON u.id = m.user_id WHERE m.org_id = ? ORDER BY u.email"))?;
            let rows = s.query_map(params![org], |r| {
                let u = user_row(r)?;
                let role: String = r.get(11)?;
                Ok(OrgMember { user: u.public(), role: OrgRole::parse(&role).unwrap_or(OrgRole::Member) })
            })?;
            Ok(rows.collect::<duckdb::Result<Vec<_>>>()?)
        })
    }

    /// Organizations a user belongs to, with their role.
    pub fn orgs_of(&self, user: &str) -> Result<Vec<(Org, OrgRole)>> {
        self.read(|c| {
            let mut s = c.prepare("SELECT o.id, o.name, o.created_at, m.role FROM org_members m JOIN orgs o ON o.id = m.org_id WHERE m.user_id = ? ORDER BY o.created_at")?;
            let rows = s.query_map(params![user], |r| {
                let role: String = r.get(3)?;
                Ok((Org { id: r.get(0)?, name: r.get(1)?, created_at: r.get::<_, i64>(2)? as u64 }, OrgRole::parse(&role).unwrap_or(OrgRole::Member)))
            })?;
            Ok(rows.collect::<duckdb::Result<Vec<_>>>()?)
        })
    }

    // ---------------------------------------------------------------- spaces

    pub fn create_space(c: &Connection, id: &str, org: &str, name: &str) -> Result<Space> {
        let name = name.trim();
        let n: i64 = c.query_row("SELECT count(*) FROM spaces WHERE org_id = ? AND lower(name) = lower(?)", params![org, name], |r| r.get(0))?;
        if n > 0 {
            bail!("a space named {name:?} already exists");
        }
        let s = Space { id: id.to_string(), org_id: org.to_string(), name: name.to_string(), created_at: now_ms(), publish_role: SpaceRole::Editor, python: true };
        c.execute("INSERT INTO spaces (id, org_id, name, created_at, publish_role, python) VALUES (?, ?, ?, ?, 'editor', true)", params![s.id, s.org_id, s.name, s.created_at as i64])?;
        Ok(s)
    }

    pub fn space(&self, id: &str) -> Result<Option<Space>> {
        self.read(|c| Self::space_in(c, id))
    }

    pub fn space_in(c: &Connection, id: &str) -> Result<Option<Space>> {
        Ok(c.query_row("SELECT id, org_id, name, created_at, publish_role, python FROM spaces WHERE id = ?", params![id], space_row).optional()?)
    }

    pub fn spaces(&self) -> Result<Vec<Space>> {
        self.read(|c| {
            let mut s = c.prepare("SELECT id, org_id, name, created_at, publish_role, python FROM spaces ORDER BY lower(name), id")?;
            let rows = s.query_map([], space_row)?;
            Ok(rows.collect::<duckdb::Result<Vec<_>>>()?)
        })
    }

    pub fn rename_space(&self, id: &str, name: &str) -> Result<Space> {
        self.txn(|c| {
            let Some(mut s) = Self::space_in(c, id)? else { bail!("space not found") };
            let name = name.trim();
            if name.is_empty() {
                bail!("name cannot be empty");
            }
            let n: i64 = c.query_row("SELECT count(*) FROM spaces WHERE org_id = ? AND lower(name) = lower(?) AND id <> ?", params![s.org_id, name, id], |r| r.get(0))?;
            if n > 0 {
                bail!("a space named {name:?} already exists");
            }
            c.execute("UPDATE spaces SET name = ? WHERE id = ?", params![name, id])?;
            s.name = name.to_string();
            Ok(s)
        })
    }

    pub fn set_space_python(&self, id: &str, allowed: bool) -> Result<()> {
        self.txn(|c| {
            c.execute("UPDATE spaces SET python = ? WHERE id = ?", params![allowed, id])?;
            Ok(())
        })
    }

    pub fn set_publish_role(&self, id: &str, role: SpaceRole) -> Result<()> {
        self.txn(|c| {
            c.execute("UPDATE spaces SET publish_role = ? WHERE id = ?", params![role.as_str(), id])?;
            Ok(())
        })
    }

    /// Remove a space record with its memberships and tokens (the caller checked it holds
    /// no workbooks).
    pub fn delete_space(&self, id: &str) -> Result<()> {
        self.txn(|c| {
            c.execute("DELETE FROM space_members WHERE space_id = ?", params![id])?;
            c.execute("DELETE FROM space_quotas WHERE space_id = ?", params![id])?;
            c.execute("DELETE FROM space_profiles WHERE space_id = ?", params![id])?;
            c.execute("DELETE FROM library_versions WHERE entry_id IN (SELECT id FROM library_entries WHERE space_id = ?)", params![id])?;
            c.execute("DELETE FROM library_entries WHERE space_id = ?", params![id])?;
            c.execute("UPDATE tokens SET revoked_at = ? WHERE space_id = ? AND revoked_at IS NULL", params![now_ms() as i64, id])?;
            c.execute("UPDATE invites SET revoked_at = ? WHERE space_id = ? AND accepted_at IS NULL AND revoked_at IS NULL", params![now_ms() as i64, id])?;
            c.execute("DELETE FROM spaces WHERE id = ?", params![id])?;
            Ok(())
        })
    }

    // ---------------------------------------------------------- space quotas

    /// A space's overrides of the default limits.
    pub fn space_quota(&self, space: &str) -> Result<crate::quota::QuotaLimits> {
        self.read(|c| {
            let q = c
                .query_row("SELECT storage_bytes, workbooks, jobs, open_workbooks, compute_memory_bytes FROM space_quotas WHERE space_id = ?", params![space], |r| {
                    let v = |i: usize| r.get::<_, Option<i64>>(i).map(|v| v.map(|v| v as u64));
                    Ok(crate::quota::QuotaLimits { storage_bytes: v(0)?, workbooks: v(1)?, jobs: v(2)?, open_workbooks: v(3)?, compute_memory_bytes: v(4)? })
                })
                .optional()?;
            Ok(q.unwrap_or_default())
        })
    }

    pub fn set_space_quota(&self, space: &str, q: &crate::quota::QuotaLimits) -> Result<()> {
        let v = |x: Option<u64>| x.map(|v| v.min(i64::MAX as u64) as i64);
        self.txn(|c| {
            c.execute(
                "INSERT INTO space_quotas (space_id, storage_bytes, workbooks, jobs, open_workbooks, compute_memory_bytes) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT (space_id) DO UPDATE SET storage_bytes = excluded.storage_bytes, workbooks = excluded.workbooks, jobs = excluded.jobs, open_workbooks = excluded.open_workbooks, compute_memory_bytes = excluded.compute_memory_bytes",
                params![space, v(q.storage_bytes), v(q.workbooks), v(q.jobs), v(q.open_workbooks), v(q.compute_memory_bytes)],
            )?;
            Ok(())
        })
    }

    /// Compute profiles granted to a space besides `standard` (7.13).
    pub fn space_profiles(&self, space: &str) -> Result<Vec<String>> {
        self.read(|c| {
            let mut stmt = c.prepare("SELECT profile FROM space_profiles WHERE space_id = ? ORDER BY profile")?;
            let rows = stmt.query_map(params![space], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<duckdb::Result<Vec<_>>>()?)
        })
    }

    pub fn set_space_profiles(&self, space: &str, profiles: &[String]) -> Result<()> {
        self.txn(|c| {
            c.execute("DELETE FROM space_profiles WHERE space_id = ?", params![space])?;
            for p in profiles {
                c.execute("INSERT INTO space_profiles VALUES (?, ?)", params![space, p])?;
            }
            Ok(())
        })
    }

    // --------------------------------------------------------- space members

    pub fn set_space_member(c: &Connection, space: &str, kind: MemberKind, principal: &str, role: SpaceRole) -> Result<()> {
        c.execute(
            "INSERT INTO space_members VALUES (?, ?, ?, ?) ON CONFLICT (space_id, kind, principal) DO UPDATE SET role = excluded.role",
            params![space, kind.as_str(), principal, role.as_str()],
        )?;
        Ok(())
    }

    pub fn remove_space_member(&self, space: &str, kind: MemberKind, principal: &str) -> Result<bool> {
        self.txn(|c| Ok(c.execute("DELETE FROM space_members WHERE space_id = ? AND kind = ? AND principal = ?", params![space, kind.as_str(), principal])? > 0))
    }

    /// Direct roles: `(kind, principal, role)` of a space.
    pub fn space_grants(&self, space: &str) -> Result<Vec<(MemberKind, String, SpaceRole)>> {
        self.read(|c| {
            let mut s = c.prepare("SELECT kind, principal, role FROM space_members WHERE space_id = ? ORDER BY kind DESC, principal")?;
            let rows = s.query_map(params![space], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
            Ok(rows
                .collect::<duckdb::Result<Vec<_>>>()?
                .into_iter()
                .filter_map(|(k, p, r)| SpaceRole::parse(&r).map(|r| (MemberKind::parse(&k), p, r)))
                .collect())
        })
    }

    // -------------------------------------------------------------- sessions

    pub fn create_session(&self, hash: &str, user: &str, expires_at: u64, via: &str) -> Result<()> {
        self.txn(|c| {
            c.execute("INSERT INTO sessions VALUES (?, ?, ?, ?, ?)", params![hash, user, now_ms() as i64, expires_at as i64, via])?;
            // Expired sessions are removed as new ones are made.
            c.execute("DELETE FROM sessions WHERE expires_at < ?", params![now_ms() as i64])?;
            Ok(())
        })
    }

    pub fn session(&self, hash: &str) -> Result<Option<Session>> {
        self.read(|c| {
            Ok(c.query_row("SELECT user_id, created_at, expires_at FROM sessions WHERE hash = ?", params![hash], |r| {
                Ok(Session { user_id: r.get(0)?, created_at: r.get::<_, i64>(1)? as u64, expires_at: r.get::<_, i64>(2)? as u64 })
            })
            .optional()?)
        })
    }

    pub fn delete_session(&self, hash: &str) -> Result<()> {
        self.txn(|c| {
            c.execute("DELETE FROM sessions WHERE hash = ?", params![hash])?;
            Ok(())
        })
    }

    /// End every session of `user` but the one hashed `keep`.
    pub fn delete_sessions_except(&self, user: &str, keep: &str) -> Result<()> {
        self.txn(|c| {
            c.execute("DELETE FROM sessions WHERE user_id = ? AND hash <> ?", params![user, keep])?;
            Ok(())
        })
    }

    // ---------------------------------------------------------------- tokens

    #[allow(clippy::too_many_arguments)]
    pub fn create_token(&self, id: &str, hash: &str, name: &str, space: &str, role: SpaceRole, by: &str, expires_at: u64) -> Result<Token> {
        self.txn(|c| {
            c.execute(
                "INSERT INTO tokens (id, hash, name, space_id, role, created_by, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                params![id, hash, name, space, role.as_str(), by, now_ms() as i64, expires_at as i64],
            )?;
            Ok(c.query_row(&format!("SELECT {TOKEN_COLS} FROM tokens WHERE id = ?"), params![id], token_row)?)
        })
    }

    /// A token and its secret's hash.
    pub fn token(&self, id: &str) -> Result<Option<(Token, String)>> {
        self.read(|c| {
            Ok(c.query_row(&format!("SELECT {TOKEN_COLS}, hash FROM tokens WHERE id = ?"), params![id], |r| Ok((token_row(r)?, r.get::<_, String>(9)?)))
                .optional()?)
        })
    }

    pub fn token_used(&self, id: &str) -> Result<()> {
        self.txn(|c| {
            c.execute("UPDATE tokens SET last_used = ? WHERE id = ?", params![now_ms() as i64, id])?;
            Ok(())
        })
    }

    pub fn tokens_of_space(&self, space: &str) -> Result<Vec<Token>> {
        self.read(|c| {
            let mut s = c.prepare(&format!("SELECT {TOKEN_COLS} FROM tokens WHERE space_id = ? ORDER BY created_at DESC"))?;
            let rows = s.query_map(params![space], token_row)?;
            Ok(rows.collect::<duckdb::Result<Vec<_>>>()?)
        })
    }

    pub fn revoke_token(&self, id: &str) -> Result<()> {
        self.txn(|c| {
            c.execute("UPDATE tokens SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL", params![now_ms() as i64, id])?;
            Ok(())
        })
    }

    // --------------------------------------------------------------- invites

    pub fn create_invite(&self, inv: &Invite, hash: &str) -> Result<()> {
        self.txn(|c| {
            c.execute(
                "INSERT INTO invites (id, hash, org_id, email, org_role, space_id, space_role, created_by, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    inv.id,
                    hash,
                    inv.org_id,
                    inv.email,
                    inv.org_role.as_str(),
                    inv.space_id,
                    inv.space_role.map(|r| r.as_str()),
                    inv.created_by,
                    inv.created_at as i64,
                    inv.expires_at as i64
                ],
            )?;
            Ok(())
        })
    }

    pub fn invite(&self, id: &str) -> Result<Option<(Invite, String)>> {
        self.read(|c| {
            Ok(c.query_row(&format!("SELECT {INVITE_COLS}, hash FROM invites WHERE id = ?"), params![id], |r| Ok((invite_row(r)?, r.get::<_, String>(11)?)))
                .optional()?)
        })
    }

    /// Open invitations for `email` (accepted, revoked and expired ones excluded).
    pub fn open_invites_for(&self, email: &str) -> Result<Vec<Invite>> {
        self.read(|c| {
            let mut s = c.prepare(&format!(
                "SELECT {INVITE_COLS} FROM invites WHERE email = ? AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at > ? ORDER BY created_at"
            ))?;
            let rows = s.query_map(params![email.to_lowercase(), now_ms() as i64], invite_row)?;
            Ok(rows.collect::<duckdb::Result<Vec<_>>>()?)
        })
    }

    pub fn invites_of_org(&self, org: &str) -> Result<Vec<Invite>> {
        self.read(|c| {
            let mut s = c.prepare(&format!("SELECT {INVITE_COLS} FROM invites WHERE org_id = ? ORDER BY created_at DESC"))?;
            let rows = s.query_map(params![org], invite_row)?;
            Ok(rows.collect::<duckdb::Result<Vec<_>>>()?)
        })
    }

    /// Apply an invitation to `user` (memberships) and mark it used, in `c`'s transaction.
    pub fn accept_invite(c: &Connection, inv: &Invite, user: &str) -> Result<()> {
        let n = c.execute(
            "UPDATE invites SET accepted_by = ?, accepted_at = ? WHERE id = ? AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at > ?",
            params![user, now_ms() as i64, inv.id, now_ms() as i64],
        )?;
        if n == 0 {
            bail!("this invitation was already used, revoked or has expired");
        }
        // An existing membership keeps the higher role.
        let current = Self::org_role_in(c, &inv.org_id, user)?;
        if current.is_none_or(|r| r < inv.org_role) {
            Self::set_org_role(c, &inv.org_id, user, inv.org_role)?;
        }
        if let (Some(space), Some(role)) = (&inv.space_id, inv.space_role) {
            if Self::space_in(c, space)?.is_some() {
                Self::set_space_member(c, space, MemberKind::User, user, role)?;
            }
        }
        Ok(())
    }

    pub fn revoke_invite(&self, id: &str) -> Result<()> {
        self.txn(|c| {
            c.execute("UPDATE invites SET revoked_at = ? WHERE id = ? AND accepted_at IS NULL AND revoked_at IS NULL", params![now_ms() as i64, id])?;
            Ok(())
        })
    }

    // ----------------------------------------------------------------- audit

    pub fn audit_in(c: &Connection, a: &Audit) -> Result<()> {
        let seq: i64 = c.query_row("SELECT coalesce(max(seq), 0) + 1 FROM audit", [], |r| r.get(0))?;
        c.execute(
            "INSERT INTO audit VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                seq,
                now_ms() as i64,
                a.actor,
                a.action,
                a.org_id,
                a.space_id,
                a.workbook,
                if a.denied { "denied" } else { "ok" },
                if a.detail.is_null() { "{}".to_string() } else { a.detail.to_string() }
            ],
        )?;
        Ok(())
    }

    pub fn audit(&self, a: &Audit) -> Result<()> {
        self.txn(|c| Self::audit_in(c, a))
    }

    /// Append records in one transaction, keeping their order and times.
    pub fn audit_many(&self, records: &[AuditRecord]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        self.txn(|c| {
            let base: i64 = c.query_row("SELECT coalesce(max(seq), 0) FROM audit", [], |r| r.get(0))?;
            let mut stmt = c.prepare("INSERT INTO audit VALUES (?, ?, ?, ?, ?, ?, ?, 'ok', ?)")?;
            for (i, r) in records.iter().enumerate() {
                let detail = if r.detail.is_null() { "{}".to_string() } else { r.detail.to_string() };
                stmt.execute(params![base + 1 + i as i64, r.at_ms as i64, r.actor, r.action, r.org_id, r.space_id, r.workbook, detail])?;
            }
            Ok(())
        })
    }

    /// Audit events, newest first: of one organization (and its spaces), or all with `None`.
    pub fn audit_events(&self, org: Option<&str>, workbook: Option<&str>, before: Option<u64>, limit: usize) -> Result<Vec<AuditEvent>> {
        self.read(|c| {
            let mut s = c.prepare(
                "SELECT seq, at_ms, actor, action, org_id, space_id, workbook, outcome, detail FROM audit
                 WHERE (?1 IS NULL OR org_id = ?1) AND (?2 IS NULL OR seq < ?2) AND (?4 IS NULL OR workbook = ?4) ORDER BY seq DESC LIMIT ?3",
            )?;
            let rows = s.query_map(params![org, before.map(|b| b as i64), limit as i64, workbook], |r| {
                let detail: String = r.get(8)?;
                Ok(AuditEvent {
                    seq: r.get::<_, i64>(0)? as u64,
                    at: r.get::<_, i64>(1)? as u64,
                    actor: r.get(2)?,
                    action: r.get(3)?,
                    org_id: r.get(4)?,
                    space_id: r.get(5)?,
                    workbook: r.get(6)?,
                    outcome: r.get(7)?,
                    detail: serde_json::from_str(&detail).unwrap_or(serde_json::Value::Null),
                })
            })?;
            Ok(rows.collect::<duckdb::Result<Vec<_>>>()?)
        })
    }
}

/// L.04 / M.07: an agent connection: a read-only credential for the MCP endpoint, scoped to
/// one space and optionally to some of its workbooks. Its secret is shown once.
#[derive(Debug, Clone, Serialize, TS)]
pub struct AgentConnection {
    pub id: String,
    pub name: String,
    pub space_id: String,
    /// Workbooks it may read; absent: every workbook of the space.
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workbooks: Option<Vec<String>>,
    pub created_by: String,
    pub created_at: u64,
    pub expires_at: u64,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<u64>,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used: Option<u64>,
}

/// One tool call of an agent connection (M.07 activity, M.08 inspectable work).
#[derive(Debug, Clone, Serialize, TS)]
pub struct AgentActivity {
    pub at_ms: u64,
    pub tool: String,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workbook: Option<String>,
    pub ok: bool,
    /// What it did: SQL, rows, result handle, or the error.
    #[ts(type = "Record<string, unknown>")]
    pub detail: serde_json::Value,
}

const AGENT_COLS: &str = "id, name, space_id, workbooks, created_by, created_at, expires_at, revoked_at, last_used";

fn agent_row(r: &duckdb::Row) -> duckdb::Result<AgentConnection> {
    let workbooks: Option<String> = r.get(3)?;
    Ok(AgentConnection {
        id: r.get(0)?,
        name: r.get(1)?,
        space_id: r.get(2)?,
        workbooks: workbooks.and_then(|w| serde_json::from_str(&w).ok()),
        created_by: r.get(4)?,
        created_at: r.get::<_, i64>(5)? as u64,
        expires_at: r.get::<_, i64>(6)? as u64,
        revoked_at: r.get::<_, Option<i64>>(7)?.map(|v| v as u64),
        last_used: r.get::<_, Option<i64>>(8)?.map(|v| v as u64),
    })
}

/// Activity rows kept per connection (oldest dropped).
pub const AGENT_ACTIVITY_KEPT: i64 = 2_000;

impl Store {
    #[allow(clippy::too_many_arguments)]
    pub fn create_agent(&self, id: &str, hash: &str, name: &str, space: &str, workbooks: Option<&[String]>, by: &str, expires_at: u64) -> Result<AgentConnection> {
        let w = workbooks.map(serde_json::to_string).transpose()?;
        self.txn(|c| {
            c.execute(
                "INSERT INTO agent_connections (id, hash, name, space_id, workbooks, created_by, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                params![id, hash, name, space, w, by, super::now_ms() as i64, expires_at as i64],
            )?;
            Ok(c.query_row(&format!("SELECT {AGENT_COLS} FROM agent_connections WHERE id = ?"), params![id], agent_row)?)
        })
    }

    pub fn agent(&self, id: &str) -> Result<Option<(AgentConnection, String)>> {
        self.read(|c| {
            Ok(c.query_row(&format!("SELECT {AGENT_COLS}, hash FROM agent_connections WHERE id = ?"), params![id], |r| Ok((agent_row(r)?, r.get::<_, String>(9)?)))
                .optional()?)
        })
    }

    pub fn agents(&self) -> Result<Vec<AgentConnection>> {
        self.read(|c| {
            let mut s = c.prepare(&format!("SELECT {AGENT_COLS} FROM agent_connections ORDER BY created_at DESC"))?;
            let rows = s.query_map([], agent_row)?;
            Ok(rows.collect::<duckdb::Result<Vec<_>>>()?)
        })
    }

    pub fn revoke_agent(&self, id: &str) -> Result<()> {
        self.txn(|c| {
            c.execute("UPDATE agent_connections SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL", params![super::now_ms() as i64, id])?;
            Ok(())
        })
    }

    /// Record one tool call (and that the connection was used).
    pub fn log_agent(&self, id: &str, tool: &str, workbook: Option<&str>, ok: bool, detail: &serde_json::Value) -> Result<()> {
        self.txn(|c| {
            let now = super::now_ms() as i64;
            c.execute("UPDATE agent_connections SET last_used = ? WHERE id = ?", params![now, id])?;
            c.execute("INSERT INTO agent_activity VALUES (?, ?, ?, ?, ?, ?)", params![id, now, tool, workbook, ok, detail.to_string()])?;
            c.execute(
                "DELETE FROM agent_activity WHERE connection_id = ? AND at_ms < (SELECT min(at_ms) FROM (SELECT at_ms FROM agent_activity WHERE connection_id = ? ORDER BY at_ms DESC LIMIT ?))",
                params![id, id, AGENT_ACTIVITY_KEPT],
            )?;
            Ok(())
        })
    }

    pub fn agent_activity(&self, id: &str, limit: usize) -> Result<Vec<AgentActivity>> {
        self.read(|c| {
            let mut s = c.prepare("SELECT at_ms, tool, workbook, ok, detail FROM agent_activity WHERE connection_id = ? ORDER BY at_ms DESC LIMIT ?")?;
            let rows = s.query_map(params![id, limit as i64], |r| {
                let detail: String = r.get(4)?;
                Ok(AgentActivity { at_ms: r.get::<_, i64>(0)? as u64, tool: r.get(1)?, workbook: r.get(2)?, ok: r.get(3)?, detail: serde_json::from_str(&detail).unwrap_or_default() })
            })?;
            Ok(rows.collect::<duckdb::Result<Vec<_>>>()?)
        })
    }
}

/// M.06: a change an agent asked for, waiting for a person to approve it. Nothing is written
/// to the workbook until then.
#[derive(Debug, Clone, Serialize, TS)]
pub struct AgentProposal {
    pub id: String,
    pub connection_id: String,
    pub workbook: String,
    /// What it would do: `saved_query`.
    pub kind: String,
    pub name: String,
    /// The SQL and parameters of a saved query.
    #[ts(type = "Record<string, unknown>")]
    pub payload: serde_json::Value,
    /// Workbook revision the agent read when it asked.
    pub revision: u64,
    pub created_at: u64,
    /// pending, approved, rejected or failed.
    pub status: String,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<u64>,
    #[ts(optional, type = "Record<string, unknown>")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
}

const PROPOSAL_COLS: &str = "id, connection_id, workbook, kind, name, payload, revision, created_at, status, decided_by, decided_at, result";

fn proposal_row(r: &duckdb::Row) -> duckdb::Result<AgentProposal> {
    let payload: String = r.get(5)?;
    let result: Option<String> = r.get(11)?;
    Ok(AgentProposal {
        id: r.get(0)?,
        connection_id: r.get(1)?,
        workbook: r.get(2)?,
        kind: r.get(3)?,
        name: r.get(4)?,
        payload: serde_json::from_str(&payload).unwrap_or_default(),
        revision: r.get::<_, i64>(6)? as u64,
        created_at: r.get::<_, i64>(7)? as u64,
        status: r.get(8)?,
        decided_by: r.get(9)?,
        decided_at: r.get::<_, Option<i64>>(10)?.map(|v| v as u64),
        result: result.and_then(|x| serde_json::from_str(&x).ok()),
    })
}

/// Pending proposals one connection may hold.
pub const MAX_PENDING_PROPOSALS: i64 = 50;

impl Store {
    /// Record a proposal; with `operation_id`, asking again returns the first one (idempotent).
    #[allow(clippy::too_many_arguments)]
    pub fn create_proposal(&self, id: &str, connection: &str, operation_id: Option<&str>, workbook: &str, kind: &str, name: &str, payload: &serde_json::Value, revision: u64) -> Result<AgentProposal> {
        self.txn(|c| {
            if let Some(op) = operation_id {
                let existing = c
                    .query_row(&format!("SELECT {PROPOSAL_COLS} FROM agent_proposals WHERE connection_id = ? AND operation_id = ?"), params![connection, op], proposal_row)
                    .optional()?;
                if let Some(p) = existing {
                    return Ok(p);
                }
            }
            let pending: i64 = c.query_row("SELECT count(*) FROM agent_proposals WHERE connection_id = ? AND status = 'pending'", params![connection], |r| r.get(0))?;
            if pending >= MAX_PENDING_PROPOSALS {
                bail!("this connection already has {MAX_PENDING_PROPOSALS} proposals waiting for approval");
            }
            c.execute(
                "INSERT INTO agent_proposals (id, connection_id, operation_id, workbook, kind, name, payload, revision, created_at, status) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending')",
                params![id, connection, operation_id, workbook, kind, name, payload.to_string(), revision as i64, super::now_ms() as i64],
            )?;
            Ok(c.query_row(&format!("SELECT {PROPOSAL_COLS} FROM agent_proposals WHERE id = ?"), params![id], proposal_row)?)
        })
    }

    pub fn proposal(&self, id: &str) -> Result<Option<AgentProposal>> {
        self.read(|c| Ok(c.query_row(&format!("SELECT {PROPOSAL_COLS} FROM agent_proposals WHERE id = ?"), params![id], proposal_row).optional()?))
    }

    pub fn proposals(&self, connection: &str) -> Result<Vec<AgentProposal>> {
        self.read(|c| {
            let mut s = c.prepare(&format!("SELECT {PROPOSAL_COLS} FROM agent_proposals WHERE connection_id = ? ORDER BY created_at DESC LIMIT 200"))?;
            let rows = s.query_map(params![connection], proposal_row)?;
            Ok(rows.collect::<duckdb::Result<Vec<_>>>()?)
        })
    }

    /// Settle a pending proposal; false when it was no longer pending.
    pub fn decide_proposal(&self, id: &str, status: &str, by: &str, result: Option<&serde_json::Value>) -> Result<bool> {
        self.txn(|c| {
            let n = c.execute(
                "UPDATE agent_proposals SET status = ?, decided_by = ?, decided_at = ?, result = ? WHERE id = ? AND status IN ('pending', 'deciding')",
                params![status, by, super::now_ms() as i64, result.map(|r| r.to_string()), id],
            )?;
            Ok(n > 0)
        })
    }

    /// Claim a pending proposal for deciding (so two approvals cannot both apply it).
    pub fn claim_proposal(&self, id: &str) -> Result<bool> {
        self.txn(|c| Ok(c.execute("UPDATE agent_proposals SET status = 'deciding' WHERE id = ? AND status = 'pending'", params![id])? > 0))
    }
}
