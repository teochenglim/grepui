//! Org administration (R-C3, R-C6): members, API tokens, branding and the audit search of one org,
//! plus platform administration: orgs (create, delete with cascade) and users.

use super::{Shared, auth::meta, ok};
use crate::{
    audit,
    auth::{self, Identity, Role, TokenRec, User, password, store as astore},
    error::ApiError,
    org::{Org, valid_name},
    query::{Bind, ident},
    store::{
        LOCAL_USER,
        build::{self, ItemRow, PLATFORM_ORG},
    },
};
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::Response,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn created(r: Response) -> Response {
    let (mut p, b) = r.into_parts();
    p.status = StatusCode::CREATED;
    Response::from_parts(p, b)
}

// ---- members ---------------------------------------------------------------------------------

pub async fn members(
    State(s): State<Shared>,
    Path(org): Path<String>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let out: Vec<Value> = auth::users(&s)
        .await?
        .into_iter()
        .filter_map(|u| u.orgs.get(&org.name).map(|r| json!({"username": u.username, "display": u.display, "role": r.name(), "source": u.source, "disabled": u.disabled})))
        .collect();
    Ok(ok(out, meta("members_list")))
}

#[derive(Deserialize)]
pub struct NewMember {
    username: String,
    role: String,
    /// For a person who does not have an account yet.
    #[serde(default)]
    password: Option<String>,
}

fn role_of(s: &str) -> Result<Role, ApiError> {
    Role::parse(s).ok_or_else(|| ApiError::bad("the role must be viewer, editor or admin"))
}

pub async fn member_add(
    State(s): State<Shared>,
    Extension(id): Extension<Identity>,
    Path(org): Path<String>,
    Json(b): Json<NewMember>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let username = b.username.trim().to_ascii_lowercase();
    let role = role_of(&b.role)?;
    if !auth::valid_username(&username) {
        return Err(ApiError::bad(
            "the username must be 1-64 characters: lowercase letters, digits, . _ - @",
        ));
    }
    let now = s.now_ms();
    let (mut u, created_ms) = match auth::user_get(&s, &username).await? {
        Some(x) => x,
        None => {
            let pw = b
                .password
                .as_deref()
                .ok_or_else(|| ApiError::bad("this person has no account yet: give a password"))?;
            auth::check_password(pw)?;
            let hash =
                password::hash(pw).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
            (
                User {
                    username: username.clone(),
                    display: username.clone(),
                    password_hash: hash,
                    source: "local".into(),
                    ..Default::default()
                },
                now,
            )
        }
    };
    u.orgs.insert(org.name.clone(), role);
    auth::user_put(&s, &u, created_ms).await?;
    audit::event(
        &s,
        &org.name,
        &id.username,
        "members.add",
        "ok",
        &format!("{username} as {}", role.name()),
    )
    .await;
    Ok(created(ok(
        json!({"username": username, "role": role.name()}),
        meta("member_add"),
    )))
}

#[derive(Deserialize)]
pub struct RoleBody {
    role: String,
}

pub async fn member_update(
    State(s): State<Shared>,
    Extension(id): Extension<Identity>,
    Path((org, user)): Path<(String, String)>,
    Json(b): Json<RoleBody>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let role = role_of(&b.role)?;
    let (mut u, created_ms) = auth::user_get(&s, &user)
        .await?
        .filter(|(u, _)| u.orgs.contains_key(&org.name))
        .ok_or_else(|| ApiError::not_found("no such member"))?;
    u.orgs.insert(org.name.clone(), role);
    auth::user_put(&s, &u, created_ms).await?;
    audit::event(
        &s,
        &org.name,
        &id.username,
        "members.role",
        "ok",
        &format!("{user} as {}", role.name()),
    )
    .await;
    Ok(ok(
        json!({"username": user, "role": role.name()}),
        meta("member_update"),
    ))
}

pub async fn member_remove(
    State(s): State<Shared>,
    Extension(id): Extension<Identity>,
    Path((org, user)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let (mut u, created_ms) = auth::user_get(&s, &user)
        .await?
        .filter(|(u, _)| u.orgs.contains_key(&org.name))
        .ok_or_else(|| ApiError::not_found("no such member"))?;
    u.orgs.remove(&org.name);
    auth::user_put(&s, &u, created_ms).await?;
    audit::event(&s, &org.name, &id.username, "members.remove", "ok", &user).await;
    Ok(ok(json!({"removed": user}), meta("member_remove")))
}

// ---- tokens ----------------------------------------------------------------------------------

pub async fn tokens(
    State(s): State<Shared>,
    Path(org): Path<String>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let out: Vec<Value> = astore::list(&s, &org.name, "token")
        .await?
        .iter()
        .filter_map(|r| {
            let t: TokenRec = serde_json::from_str(&r.spec).ok()?;
            Some(json!({"id": r.item_id, "name": t.name, "role": t.role.name(), "created_by": t.created_by, "created_ms": r.created_ms, "expires_ms": t.expires_ms}))
        })
        .collect();
    Ok(ok(out, meta("tokens_list")))
}

#[derive(Deserialize)]
pub struct NewToken {
    name: String,
    role: String,
    #[serde(default)]
    expires_days: Option<i64>,
}

/// A new token. It is shown here once; only its SHA-256 is stored.
pub async fn token_create(
    State(s): State<Shared>,
    Extension(id): Extension<Identity>,
    Path(org): Path<String>,
    Json(b): Json<NewToken>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let name = b.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 60 || name.chars().any(char::is_control) {
        return Err(ApiError::bad("the name must be 1-60 characters"));
    }
    let role = role_of(&b.role)?;
    if id.role_in(&org.name).is_none_or(|mine| role > mine) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "a token cannot have more rights than you".into(),
        ));
    }
    let now = s.now_ms();
    let expires = match b.expires_days {
        Some(d) if (1..=3650).contains(&d) => Some(now + d * 86_400_000),
        Some(_) => return Err(ApiError::bad("expires_days must be 1-3650")),
        None => None,
    };
    let token = auth::new_token(&org.name);
    let key = auth::sha_hex(&token);
    let rec = TokenRec {
        name: name.clone(),
        role,
        created_by: id.username.clone(),
        expires_ms: expires,
    };
    let m = astore::put(
        &s,
        &org.name,
        "token",
        &astore::row(&key, &name, &rec, &id.username, now, now),
    )
    .await?;
    audit::event(
        &s,
        &org.name,
        &id.username,
        "tokens.create",
        "ok",
        &format!("{name} as {}", role.name()),
    )
    .await;
    Ok(created(ok(
        json!({"id": key, "name": name, "role": role.name(), "token": token, "note": "this is the only time the token is shown"}),
        m,
    )))
}

pub async fn token_revoke(
    State(s): State<Shared>,
    Extension(id): Extension<Identity>,
    Path((org, tid)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let row = astore::get(&s, &org.name, "token", &tid)
        .await?
        .ok_or_else(|| ApiError::not_found("no such token"))?;
    let m = astore::remove(&s, &org.name, "token", row, s.now_ms()).await?;
    s.auth.forget(&tid);
    audit::event(
        &s,
        &org.name,
        &id.username,
        "tokens.revoke",
        "ok",
        &tid.chars().take(12).collect::<String>(),
    )
    .await;
    Ok(ok(json!({"revoked": true}), m))
}

// ---- branding --------------------------------------------------------------------------------

fn branding_of(r: Option<&ItemRow>) -> Value {
    r.and_then(|r| serde_json::from_str::<Value>(&r.spec).ok())
        .map(|v| v["branding"].clone())
        .filter(|b| b.is_object())
        .unwrap_or(json!({}))
}

pub async fn branding(
    State(s): State<Shared>,
    Path(org): Path<String>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let row = astore::get(&s, PLATFORM_ORG, "orgdef", &org.name).await?;
    let mut b = branding_of(row.as_ref());
    if b["name"].is_null() {
        b["name"] = json!(org.name);
    }
    Ok(ok(b, meta("branding_get")))
}

#[derive(Deserialize)]
pub struct Branding {
    #[serde(default)]
    name: String,
    #[serde(default)]
    logo_url: String,
    #[serde(default)]
    accent: String,
}

fn check_branding(b: &Branding) -> Result<(), ApiError> {
    if b.name.chars().count() > 60 || b.name.chars().any(char::is_control) {
        return Err(ApiError::bad("the name is at most 60 characters"));
    }
    if !b.logo_url.is_empty() {
        let u = url::Url::parse(&b.logo_url)
            .map_err(|_| ApiError::bad("the logo must be an http(s) URL"))?;
        if !matches!(u.scheme(), "http" | "https") || b.logo_url.len() > 300 {
            return Err(ApiError::bad(
                "the logo must be an http(s) URL of at most 300 characters",
            ));
        }
    }
    let c = b.accent.as_bytes();
    if !b.accent.is_empty()
        && !(c.len() == 7 && c[0] == b'#' && c[1..].iter().all(u8::is_ascii_hexdigit))
    {
        return Err(ApiError::bad("the accent colour looks like #1a8f7a"));
    }
    Ok(())
}

pub async fn branding_set(
    State(s): State<Shared>,
    Extension(id): Extension<Identity>,
    Path(org): Path<String>,
    Json(b): Json<Branding>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    check_branding(&b)?;
    let now = s.now_ms();
    let old = astore::get(&s, PLATFORM_ORG, "orgdef", &org.name).await?;
    let mut spec: Value = old
        .as_ref()
        .and_then(|r| serde_json::from_str(&r.spec).ok())
        .unwrap_or(json!({}));
    spec["branding"] = json!({"name": b.name.trim(), "logo_url": b.logo_url, "accent": b.accent});
    let row = astore::row(
        &org.name,
        &org.name,
        &spec,
        &id.username,
        old.as_ref().map_or(now, |r| r.created_ms),
        now,
    );
    let m = astore::put(&s, PLATFORM_ORG, "orgdef", &row).await?;
    audit::event(&s, &org.name, &id.username, "branding.set", "ok", "").await;
    Ok(ok(spec["branding"].clone(), m))
}

// ---- audit search ----------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AuditParams {
    user: Option<String>,
    action: Option<String>,
    from: Option<i64>,
    to: Option<i64>,
    limit: Option<u32>,
}

async fn audit_search(s: &Shared, org: &str, p: AuditParams) -> Result<Response, ApiError> {
    let now = s.now_ms();
    let (from, to) = (p.from.unwrap_or(now - 86_400_000), p.to.unwrap_or(now + 1));
    if from < 0 || to <= from || to - from > crate::query::MAX_WINDOW_MS {
        return Err(ApiError::bad(
            "from and to must be a window of at most 31 days",
        ));
    }
    let q = build::audit_search(
        s.meta_db(),
        org,
        p.user.as_deref(),
        p.action.as_deref(),
        from,
        to,
        p.limit.unwrap_or(100),
    )
    .map_err(|_| ApiError::bad("invalid request"))?;
    let (rows, m) = s.read_store(q).await?;
    Ok(ok(rows, m))
}

pub async fn audit_org(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<AuditParams>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    // Queued rows are written first, so what just happened is in the answer.
    s.audit.flush(&s).await;
    audit_search(&s, &org.name, p).await
}

pub async fn audit_platform(
    State(s): State<Shared>,
    Query(p): Query<AuditParams>,
) -> Result<Response, ApiError> {
    s.audit.flush(&s).await;
    audit_search(&s, PLATFORM_ORG, p).await
}

// ---- platform: orgs --------------------------------------------------------------------------

fn org_json(o: &Org) -> Value {
    json!({"name": o.name, "database": o.database, "traces_table": o.traces_table, "logs_table": o.logs_table})
}

pub async fn platform_orgs(State(s): State<Shared>) -> Result<Response, ApiError> {
    Ok(ok(
        s.orgs
            .list()
            .iter()
            .map(|o| org_json(o))
            .collect::<Vec<_>>(),
        meta("platform_orgs"),
    ))
}

#[derive(Deserialize)]
pub struct NewOrg {
    name: String,
    #[serde(default)]
    database: Option<String>,
    #[serde(default)]
    traces_table: Option<String>,
    #[serde(default)]
    logs_table: Option<String>,
    /// Also run `CREATE DATABASE IF NOT EXISTS` (needs admin writes without sign-in).
    #[serde(default)]
    create_database: bool,
    /// An existing person who becomes the org's admin.
    #[serde(default)]
    admin: Option<String>,
}

/// Databases an org may never be mapped to.
const RESERVED_DBS: &[&str] = &["information_schema", "greptime_private", "pg_catalog"];

pub async fn platform_org_create(
    State(s): State<Shared>,
    Extension(id): Extension<Identity>,
    Json(b): Json<NewOrg>,
) -> Result<Response, ApiError> {
    let name = b.name.trim().to_string();
    if !valid_name(&name) {
        return Err(ApiError::bad(
            "the org name is 1-32 lowercase letters, digits, - or _",
        ));
    }
    if s.orgs.resolve(&name).is_ok() {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "an org with that name exists".into(),
        ));
    }
    let database = b
        .database
        .clone()
        .unwrap_or_else(|| format!("org_{}", name.replace('-', "_")));
    let (traces, logs) = (
        b.traces_table.unwrap_or_else(|| s.cfg.traces_table.clone()),
        b.logs_table.unwrap_or_else(|| s.cfg.logs_table.clone()),
    );
    for (what, v) in [
        ("database", &database),
        ("traces table", &traces),
        ("logs table", &logs),
    ] {
        ident(v).map_err(|_| ApiError::bad(format!("the {what} name is not valid")))?;
    }
    if RESERVED_DBS.contains(&database.as_str()) || database == s.cfg.meta_database {
        return Err(ApiError::bad("that database cannot be used by an org"));
    }
    if s.orgs.list().iter().any(|o| o.database == database) {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "another org already uses that database".into(),
        ));
    }
    let admin = match &b.admin {
        Some(a) => Some(
            auth::user_get(&s, a)
                .await?
                .ok_or_else(|| ApiError::bad("the admin must be an existing user"))?,
        ),
        None => None,
    };
    if b.create_database {
        s.platform_ddl(&format!("CREATE DATABASE IF NOT EXISTS `{database}`"))
            .await?;
    }
    let now = s.now_ms();
    let old = astore::get(&s, PLATFORM_ORG, "orgdef", &name).await?;
    let mut spec: Value = old
        .as_ref()
        .and_then(|r| serde_json::from_str(&r.spec).ok())
        .unwrap_or(json!({}));
    spec["database"] = json!(database);
    spec["traces_table"] = json!(traces);
    spec["logs_table"] = json!(logs);
    let m = astore::put(
        &s,
        PLATFORM_ORG,
        "orgdef",
        &astore::row(&name, &name, &spec, &id.username, now, now),
    )
    .await?;
    let org = Org {
        name: name.clone(),
        database,
        traces_table: traces,
        logs_table: logs,
    };
    s.orgs.set(org.clone());
    if let Some((mut u, c)) = admin {
        u.orgs.insert(name.clone(), Role::Admin);
        auth::user_put(&s, &u, c).await?;
    }
    audit::event(
        &s,
        PLATFORM_ORG,
        &id.username,
        "platform.org.create",
        "ok",
        &format!("{name} -> {}", org.database),
    )
    .await;
    Ok(created(ok(org_json(&org), m)))
}

/// Soft-delete every row of `org` in every state table. Telemetry is never touched.
async fn cascade(s: &Shared, org: &str, now: i64) -> Result<Value, ApiError> {
    let mut counts = serde_json::Map::new();
    let mut bump = |k: &str, n: usize| {
        counts.insert(k.into(), json!(n));
    };
    let db = s.meta_db().to_string();
    let bad = |_| ApiError::bad("invalid request");
    // Saved views, service notes, folders, dashboards (and playlists), public links.
    let (views, _) = s
        .read_store(build::view_list(&db, org, None).map_err(bad)?)
        .await?;
    for v in &views {
        let r = build::ViewRow {
            view_id: v["view_id"].as_str().unwrap_or_default().into(),
            owner: v["owner"].as_str().unwrap_or_default().into(),
            name: v["name"].as_str().unwrap_or_default().into(),
            kind: v["kind"].as_str().unwrap_or_default().into(),
            spec: v["spec"].as_str().unwrap_or_default().into(),
            created_ms: v["created_ms"].as_i64().unwrap_or(0),
            updated_ms: now,
            deleted: true,
        };
        s.write(build::view_put(&db, org, &r).map_err(bad)?).await?;
    }
    bump("views", views.len());
    let (notes, _) = s
        .read_store(build::service_notes(&db, org).map_err(bad)?)
        .await?;
    for n in &notes {
        let r = build::ServiceNote {
            service: n["service"].as_str().unwrap_or_default().into(),
            updated_ms: now,
            deleted: true,
            ..Default::default()
        };
        s.write(build::service_note_put(&db, org, &r).map_err(bad)?)
            .await?;
    }
    bump("service_notes", notes.len());
    let (dashes, _) = s
        .read_store(build::dash_list(&db, org, None, None).map_err(bad)?)
        .await?;
    for d in &dashes {
        let id = d["dash_id"].as_str().unwrap_or_default();
        let (shares, _) = s
            .read_store(build::share_list(&db, org, id).map_err(bad)?)
            .await?;
        for sh in &shares {
            let r = build::ShareRow {
                share_id: sh["share_id"].as_str().unwrap_or_default().into(),
                dash_id: id.into(),
                token_hash: sh["token_hash"].as_str().unwrap_or_default().into(),
                created_by: sh["created_by"].as_str().unwrap_or_default().into(),
                created_ms: sh["created_ms"].as_i64().unwrap_or(0),
                expires_ms: sh["expires_ms"].as_i64().unwrap_or(0),
                revoked: true,
            };
            s.write(build::share_put(&db, org, &r).map_err(bad)?)
                .await?;
        }
        let r = build::DashRow {
            dash_id: id.into(),
            folder_id: d["folder_id"].as_str().unwrap_or_default().into(),
            title: d["title"].as_str().unwrap_or_default().into(),
            kind: d["kind"].as_str().unwrap_or_default().into(),
            spec: String::new(),
            owner: d["owner"].as_str().unwrap_or_default().into(),
            created_ms: d["created_ms"].as_i64().unwrap_or(0),
            updated_ms: now,
            deleted: true,
        };
        s.write(build::dash_put(&db, org, &r).map_err(bad)?).await?;
    }
    bump("dashboards", dashes.len());
    let (folders, _) = s
        .read_store(build::folder_list(&db, org).map_err(bad)?)
        .await?;
    for f in &folders {
        let r = build::FolderRow {
            folder_id: f["folder_id"].as_str().unwrap_or_default().into(),
            name: f["name"].as_str().unwrap_or_default().into(),
            owner: f["owner"].as_str().unwrap_or_default().into(),
            created_ms: f["created_ms"].as_i64().unwrap_or(0),
            updated_ms: now,
            deleted: true,
        };
        s.write(build::folder_put(&db, org, &r).map_err(bad)?)
            .await?;
    }
    bump("folders", folders.len());
    // Everything kept as kind-tagged items: alerts, destinations, functions, tokens, ...
    let mut items = 0;
    for kind in [
        "alert",
        "destination",
        "downtime",
        "incident",
        "state",
        "lease",
        "function",
        "redaction",
        "token",
    ] {
        for r in astore::list(s, org, kind).await? {
            astore::remove(s, org, kind, r, now).await?;
            items += 1;
        }
    }
    bump("items", items);
    // People lose their membership of the org.
    let mut members = 0;
    for mut u in auth::users(s).await? {
        if u.orgs.remove(org).is_some() {
            let created = auth::user_get(s, &u.username)
                .await?
                .map_or(now, |(_, c)| c);
            auth::user_put(s, &u, created).await?;
            members += 1;
        }
    }
    bump("members", members);
    if let Some(r) = astore::get(s, PLATFORM_ORG, "orgdef", org).await? {
        astore::remove(s, PLATFORM_ORG, "orgdef", r, now).await?;
    }
    Ok(Value::Object(counts))
}

pub async fn platform_org_delete(
    State(s): State<Shared>,
    Extension(id): Extension<Identity>,
    Path(org): Path<String>,
) -> Result<Response, ApiError> {
    if org == crate::org::DEFAULT_ORG {
        return Err(ApiError::bad(
            "the default org comes from the flags and cannot be deleted",
        ));
    }
    let o = s.org(&org)?;
    let counts = cascade(&s, &o.name, s.now_ms()).await?;
    s.orgs.remove(&o.name);
    s.redact.write().ok().map(|mut m| m.remove(&o.name));
    s.auth.forget_all();
    audit::event(
        &s,
        PLATFORM_ORG,
        &id.username,
        "platform.org.delete",
        "ok",
        &format!("{} {counts}", o.name),
    )
    .await;
    Ok(ok(
        json!({"deleted": o.name, "removed": counts, "telemetry": "untouched"}),
        meta("platform_org_delete"),
    ))
}

/// Make the orgs of the state store the orgs of this process (startup and a periodic refresh).
pub async fn reload_orgs(s: &Shared) -> Result<(), ApiError> {
    let rows = match astore::list(s, PLATFORM_ORG, "orgdef").await {
        Ok(r) => r,
        Err(e) if e.0 == StatusCode::SERVICE_UNAVAILABLE => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut wanted = vec![crate::org::DEFAULT_ORG.to_string()];
    for r in rows {
        let v: Value = serde_json::from_str(&r.spec).unwrap_or_default();
        let (Some(db), Some(t), Some(l)) = (
            v["database"].as_str(),
            v["traces_table"].as_str(),
            v["logs_table"].as_str(),
        ) else {
            continue;
        };
        if !valid_name(&r.item_id)
            || r.item_id == crate::org::DEFAULT_ORG
            || ident(db).is_err()
            || ident(t).is_err()
            || ident(l).is_err()
        {
            continue;
        }
        wanted.push(r.item_id.clone());
        s.orgs.set(Org {
            name: r.item_id.clone(),
            database: db.into(),
            traces_table: t.into(),
            logs_table: l.into(),
        });
    }
    for o in s.orgs.list() {
        if !wanted.contains(&o.name) {
            s.orgs.remove(&o.name);
        }
    }
    Ok(())
}

// ---- platform: users ---------------------------------------------------------------------------

pub async fn platform_users(State(s): State<Shared>) -> Result<Response, ApiError> {
    let out: Vec<Value> = auth::users(&s).await?.iter().map(|u| json!({"username": u.username, "display": u.display, "platform_admin": u.platform_admin, "disabled": u.disabled, "source": u.source, "orgs": u.orgs.iter().map(|(o, r)| (o.clone(), json!(r.name()))).collect::<serde_json::Map<_, _>>()})).collect();
    Ok(ok(out, meta("platform_users")))
}

#[derive(Deserialize)]
pub struct NewUser {
    username: String,
    password: String,
    #[serde(default)]
    platform_admin: bool,
    #[serde(default)]
    orgs: BTreeMap<String, String>,
}

pub async fn platform_user_create(
    State(s): State<Shared>,
    Extension(id): Extension<Identity>,
    Json(b): Json<NewUser>,
) -> Result<Response, ApiError> {
    let username = b.username.trim().to_ascii_lowercase();
    if !auth::valid_username(&username) {
        return Err(ApiError::bad(
            "the username must be 1-64 characters: lowercase letters, digits, . _ - @",
        ));
    }
    if auth::user_get(&s, &username).await?.is_some() {
        return Err(ApiError(StatusCode::CONFLICT, "that user exists".into()));
    }
    auth::check_password(&b.password)?;
    let mut orgs = BTreeMap::new();
    for (o, r) in &b.orgs {
        s.org(o)?;
        orgs.insert(o.clone(), role_of(r)?);
    }
    let hash =
        password::hash(&b.password).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let u = User {
        username: username.clone(),
        display: username.clone(),
        password_hash: hash,
        platform_admin: b.platform_admin,
        disabled: false,
        source: "local".into(),
        orgs,
    };
    auth::user_put(&s, &u, s.now_ms()).await?;
    audit::event(
        &s,
        PLATFORM_ORG,
        &id.username,
        "platform.user.create",
        "ok",
        &username,
    )
    .await;
    Ok(created(ok(
        json!({"username": username}),
        meta("platform_user_create"),
    )))
}

#[derive(Deserialize)]
pub struct UserPatch {
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    disabled: Option<bool>,
    #[serde(default)]
    platform_admin: Option<bool>,
}

pub async fn platform_user_update(
    State(s): State<Shared>,
    Extension(id): Extension<Identity>,
    Path(user): Path<String>,
    Json(b): Json<UserPatch>,
) -> Result<Response, ApiError> {
    let (mut u, created_ms) = auth::user_get(&s, &user)
        .await?
        .ok_or_else(|| ApiError::not_found("no such user"))?;
    if let Some(p) = &b.password {
        auth::check_password(p)?;
        u.password_hash =
            password::hash(p).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    }
    if let Some(d) = b.disabled {
        u.disabled = d;
    }
    if let Some(a) = b.platform_admin {
        u.platform_admin = a;
    }
    auth::user_put(&s, &u, created_ms).await?;
    audit::event(
        &s,
        PLATFORM_ORG,
        &id.username,
        "platform.user.update",
        "ok",
        &user,
    )
    .await;
    Ok(ok(json!({"username": user}), meta("platform_user_update")))
}

pub async fn platform_user_delete(
    State(s): State<Shared>,
    Extension(id): Extension<Identity>,
    Path(user): Path<String>,
) -> Result<Response, ApiError> {
    if user == id.username {
        return Err(ApiError::bad("you cannot delete yourself"));
    }
    let row = astore::get(&s, PLATFORM_ORG, "user", &user)
        .await?
        .ok_or_else(|| ApiError::not_found("no such user"))?;
    astore::remove(&s, PLATFORM_ORG, "user", row, s.now_ms()).await?;
    s.auth.forget_all();
    audit::event(
        &s,
        PLATFORM_ORG,
        &id.username,
        "platform.user.delete",
        "ok",
        &user,
    )
    .await;
    Ok(ok(json!({"deleted": user}), meta("platform_user_delete")))
}

pub fn _unused(_: Bind, _: &str) -> &'static str {
    LOCAL_USER
}

/// Keep the orgs fresh across replicas.
pub fn spawn_org_refresh(s: Shared) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            if let Err(e) = reload_orgs(&s).await {
                tracing::warn!(error = %e.1, "could not reload orgs");
            }
        }
    });
}
