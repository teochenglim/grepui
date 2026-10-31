//! Dashboards (R-B1 – R-B6): folders, dashboards and playlists, panel data, variables, import and
//! export, and public links. Panels run their saved queries here; the browser never sends SQL.

use super::{Shared, ok};
use crate::{
    dash::{
        self, panels, time,
        vars::{self, Selected},
    },
    error::{ApiError, ReadError},
    org::Org,
    query::{Bind, Meta, Q, Window, metrics, run_max, sqlmode},
    store::{
        LOCAL_USER,
        build::{self, DashRow, FolderRow, ShareRow},
    },
};
use axum::{
    Json,
    extract::{Path, Query, RawQuery, State},
    http::StatusCode,
    response::Response,
};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const PANEL_ROWS: usize = 2_000;
const VARIABLE_VALUES: usize = 200;
const MAX_SHARE_HOURS: i64 = 24 * 90;
const DAY_KIND: &str = "dashboard";
const PLAYLIST_KIND: &str = "playlist";

fn invalid<T>(_: T) -> ApiError {
    ApiError::bad("invalid request")
}

fn new_id() -> String {
    format!("{:016x}", rand::random::<u64>())
}

fn hex_id(id: &str, what: &str) -> Result<String, ApiError> {
    if id.len() == 16 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        Ok(id.to_string())
    } else {
        Err(ApiError::bad(format!("invalid {what} id")))
    }
}

fn name(n: &str, what: &str) -> Result<String, ApiError> {
    let n = n.trim();
    if n.is_empty() || n.chars().count() > dash::MAX_TITLE || n.chars().any(char::is_control) {
        return Err(ApiError::bad(format!(
            "{what} must be 1-100 characters, without control characters"
        )));
    }
    Ok(n.to_string())
}

fn static_meta(name: &str) -> Meta {
    Meta {
        query: name.into(),
        sql: String::new(),
        binds: vec![],
        ms: 0,
        rows: 0,
        source: "raw",
        note: None,
    }
}

fn status(code: StatusCode, resp: Response) -> Response {
    let (mut parts, body) = resp.into_parts();
    parts.status = code;
    Response::from_parts(parts, body)
}

// ---- folders ----------------------------------------------------------------------------

fn folder_json(r: &Value) -> Value {
    json!({"id": r["folder_id"], "name": r["name"], "owner": r["owner"], "created_ms": r["created_ms"], "updated_ms": r["updated_ms"]})
}

async fn folders(s: &Shared, org: &Org) -> Result<(Vec<Value>, Meta), ApiError> {
    s.read_store(build::folder_list(s.meta_db(), &org.name).map_err(invalid)?)
        .await
}

pub async fn folder_list(
    State(s): State<Shared>,
    Path(org): Path<String>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let (rows, meta) = folders(&s, org).await?;
    Ok(ok(rows.iter().map(folder_json).collect::<Vec<_>>(), meta))
}

#[derive(Deserialize)]
pub struct FolderBody {
    name: String,
}

pub async fn folder_create(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Json(b): Json<FolderBody>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let now = s.now_ms();
    let row = FolderRow {
        folder_id: new_id(),
        name: name(&b.name, "name")?,
        owner: LOCAL_USER.into(),
        created_ms: now,
        updated_ms: now,
        deleted: false,
    };
    let meta = s
        .write(build::folder_put(s.meta_db(), &org.name, &row).map_err(invalid)?)
        .await?;
    Ok(status(
        StatusCode::CREATED,
        ok(
            json!({"id": row.folder_id, "name": row.name, "owner": row.owner, "created_ms": now, "updated_ms": now}),
            meta,
        ),
    ))
}

async fn folder_row(s: &Shared, org: &Org, id: &str) -> Result<FolderRow, ApiError> {
    let (rows, _) = folders(s, org).await?;
    let r = rows
        .iter()
        .find(|r| r["folder_id"] == id)
        .ok_or_else(|| ApiError::not_found("no such folder"))?;
    Ok(FolderRow {
        folder_id: id.into(),
        name: r["name"].as_str().unwrap_or_default().into(),
        owner: r["owner"].as_str().unwrap_or(LOCAL_USER).into(),
        created_ms: r["created_ms"].as_i64().unwrap_or(0),
        updated_ms: r["updated_ms"].as_i64().unwrap_or(0),
        deleted: false,
    })
}

pub async fn folder_rename(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
    Json(b): Json<FolderBody>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let mut row = folder_row(&s, org, &hex_id(&id, "folder")?).await?;
    row.name = name(&b.name, "name")?;
    row.updated_ms = s.now_ms();
    let meta = s
        .write(build::folder_put(s.meta_db(), &org.name, &row).map_err(invalid)?)
        .await?;
    Ok(ok(
        json!({"id": row.folder_id, "name": row.name, "updated_ms": row.updated_ms}),
        meta,
    ))
}

/// Deleting a folder deletes the dashboards in it (and only those).
pub async fn folder_delete(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let id = hex_id(&id, "folder")?;
    let mut row = folder_row(&s, org, &id).await?;
    let (inside, _) = s
        .read_store(build::dash_list(s.meta_db(), &org.name, Some(&id), None).map_err(invalid)?)
        .await?;
    let mut removed = 0;
    for d in &inside {
        let (full, _) = s
            .read_store(
                build::dash_get(
                    s.meta_db(),
                    &org.name,
                    d["dash_id"].as_str().unwrap_or_default(),
                )
                .map_err(invalid)?,
            )
            .await?;
        if let Some(r) = full.first() {
            let mut dr = dash_row(r);
            dr.deleted = true;
            dr.updated_ms = s.now_ms();
            s.write(build::dash_put(s.meta_db(), &org.name, &dr).map_err(invalid)?)
                .await?;
            removed += 1;
        }
    }
    row.deleted = true;
    row.updated_ms = s.now_ms();
    let meta = s
        .write(build::folder_put(s.meta_db(), &org.name, &row).map_err(invalid)?)
        .await?;
    Ok(ok(
        json!({"deleted": true, "id": id, "dashboards_deleted": removed}),
        meta,
    ))
}

// ---- dashboards -------------------------------------------------------------------------

fn dash_row(r: &Value) -> DashRow {
    DashRow {
        dash_id: r["dash_id"].as_str().unwrap_or_default().into(),
        folder_id: r["folder_id"].as_str().unwrap_or_default().into(),
        title: r["title"].as_str().unwrap_or_default().into(),
        kind: r["kind"].as_str().unwrap_or(DAY_KIND).into(),
        spec: r["spec"].as_str().unwrap_or("{}").into(),
        owner: r["owner"].as_str().unwrap_or(LOCAL_USER).into(),
        created_ms: r["created_ms"].as_i64().unwrap_or(0),
        updated_ms: r["updated_ms"].as_i64().unwrap_or(0),
        deleted: false,
    }
}

fn dash_json(d: &DashRow) -> Value {
    json!({
        "id": d.dash_id, "folder_id": d.folder_id, "title": d.title, "kind": d.kind, "owner": d.owner,
        "created_ms": d.created_ms, "updated_ms": d.updated_ms,
        "spec": serde_json::from_str::<Value>(&d.spec).unwrap_or(Value::Null),
    })
}

async fn load(s: &Shared, org: &Org, id: &str, kind: &str) -> Result<DashRow, ApiError> {
    let (rows, _) = s
        .read_store(build::dash_get(s.meta_db(), &org.name, &hex_id(id, kind)?).map_err(invalid)?)
        .await?;
    let row = dash_row(
        rows.first()
            .ok_or_else(|| ApiError::not_found(format!("no such {kind}")))?,
    );
    if row.kind != kind {
        return Err(ApiError::not_found(format!("no such {kind}")));
    }
    Ok(row)
}

async fn put(s: &Shared, org: &Org, d: &DashRow) -> Result<Meta, ApiError> {
    s.write(build::dash_put(s.meta_db(), &org.name, d).map_err(invalid)?)
        .await
}

/// A folder id that exists, or the root (`""`).
async fn check_folder(s: &Shared, org: &Org, id: &str) -> Result<String, ApiError> {
    if id.is_empty() {
        return Ok(String::new());
    }
    let id = hex_id(id, "folder")?;
    folder_row(s, org, &id).await?;
    Ok(id)
}

#[derive(Deserialize)]
pub struct ListParams {
    folder: Option<String>,
}

fn list_json(rows: &[Value]) -> Vec<Value> {
    rows.iter()
        .map(|r| json!({"id": r["dash_id"], "folder_id": r["folder_id"], "title": r["title"], "kind": r["kind"], "owner": r["owner"], "created_ms": r["created_ms"], "updated_ms": r["updated_ms"]}))
        .collect()
}

pub async fn dash_list(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<ListParams>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let folder = p
        .folder
        .as_deref()
        .map(|f| hex_id(f, "folder"))
        .transpose()?;
    let (rows, meta) = s
        .read_store(
            build::dash_list(s.meta_db(), &org.name, folder.as_deref(), Some(DAY_KIND))
                .map_err(invalid)?,
        )
        .await?;
    Ok(ok(list_json(&rows), meta))
}

#[derive(Deserialize)]
pub struct NewDash {
    title: String,
    folder_id: Option<String>,
    spec: Option<Value>,
}

fn empty_spec() -> Value {
    json!({"tabs": [{"title": "Main", "panels": []}]})
}

pub async fn dash_create(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Json(b): Json<NewDash>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let spec = dash::canonical(&b.spec.unwrap_or_else(empty_spec))?;
    let now = s.now_ms();
    let row = DashRow {
        dash_id: new_id(),
        folder_id: check_folder(&s, org, b.folder_id.as_deref().unwrap_or_default()).await?,
        title: name(&b.title, "title")?,
        kind: DAY_KIND.into(),
        spec: spec.to_string(),
        owner: LOCAL_USER.into(),
        created_ms: now,
        updated_ms: now,
        deleted: false,
    };
    let meta = put(&s, org, &row).await?;
    Ok(status(StatusCode::CREATED, ok(dash_json(&row), meta)))
}

pub async fn dash_get(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let (rows, meta) = s
        .read_store(
            build::dash_get(s.meta_db(), &org.name, &hex_id(&id, "dashboard")?).map_err(invalid)?,
        )
        .await?;
    let row = dash_row(
        rows.first()
            .ok_or_else(|| ApiError::not_found("no such dashboard"))?,
    );
    if row.kind != DAY_KIND {
        return Err(ApiError::not_found("no such dashboard"));
    }
    Ok(ok(dash_json(&row), meta))
}

#[derive(Deserialize)]
pub struct EditDash {
    title: Option<String>,
    folder_id: Option<String>,
    spec: Option<Value>,
}

pub async fn dash_update(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
    Json(b): Json<EditDash>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let mut row = load(&s, org, &id, DAY_KIND).await?;
    if let Some(t) = &b.title {
        row.title = name(t, "title")?;
    }
    if let Some(f) = &b.folder_id {
        row.folder_id = check_folder(&s, org, f).await?;
    }
    if let Some(sp) = &b.spec {
        row.spec = dash::canonical(sp)?.to_string();
    }
    row.updated_ms = s.now_ms();
    let meta = put(&s, org, &row).await?;
    Ok(ok(dash_json(&row), meta))
}

pub async fn dash_delete(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let mut row = load(&s, org, &id, DAY_KIND).await?;
    row.deleted = true;
    row.updated_ms = s.now_ms();
    let meta = put(&s, org, &row).await?;
    Ok(ok(json!({"deleted": true, "id": row.dash_id}), meta))
}

// ---- panel data and variables -------------------------------------------------------------

struct DataQuery {
    window: Window,
    compare: Option<String>,
    selected: Selected,
}

fn data_query(raw: Option<&str>, now_ms: i64) -> Result<DataQuery, ApiError> {
    let raw = raw.unwrap_or_default();
    let get = |k: &str| {
        url::form_urlencoded::parse(raw.as_bytes())
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.into_owned())
    };
    let num = |k: &str| {
        get(k)
            .map(|v| {
                v.parse::<i64>()
                    .map_err(|_| ApiError::bad(format!("{k} must be epoch milliseconds")))
            })
            .transpose()
    };
    let (from, to) = (num("from")?, num("to")?);
    let window = if from.is_none() && to.is_none() {
        Window::parse(Some(now_ms - 3_600_000), Some(now_ms), None)?
    } else {
        Window::parse(from, to, None)?
    };
    Ok(DataQuery {
        window,
        compare: get("compare").filter(|c| !c.is_empty()),
        selected: vars::selected_from_query(raw)?,
    })
}

/// Run a panel's saved query for a window; rows in the shape the panel type expects.
async fn run_query(
    s: &Shared,
    org: &Org,
    ty: &str,
    q: &Value,
    decls: &[Value],
    selected: &Selected,
    w: Window,
) -> Result<(Vec<Value>, Meta), ApiError> {
    let text = q["text"].as_str().unwrap_or_default();
    match q["kind"].as_str() {
        Some("sql") => {
            let (sql, binds) = vars::bind_sql(text, decls, selected)?;
            let sql = sqlmode::expand_macros(&sql, w.from_ms, w.to_ms);
            sqlmode::check(&sql, &org.database).map_err(|r| ApiError::bad(r.0))?;
            Ok(run_max(
                s.reader.as_ref(),
                Q::new("dashboard_panel", &sql, binds),
                PANEL_ROWS,
            )
            .await?)
        }
        Some("promql") => {
            let expr = vars::substitute_promql(text, decls, selected)?;
            metrics::check(&expr)?;
            if s.cfg.database().ok().as_deref() != Some(org.database.as_str()) {
                return Err(ApiError::bad(
                    "PromQL is only available for the org on the connection's database",
                ));
            }
            let grid = metrics::Grid::range(w, None);
            let (rows, mut meta) = run_max(
                s.reader.as_ref(),
                metrics::tql(&expr, grid),
                metrics::MAX_SAMPLES,
            )
            .await
            .map_err(|e| match e {
                ReadError::Db(d) => {
                    tracing::warn!(detail = %d, "dashboard PromQL failed");
                    ApiError::bad("the database could not evaluate this panel's PromQL")
                }
                other => other.into(),
            })?;
            let series = metrics::to_series(&rows)?;
            let out = panels::promql_rows(ty, &series);
            meta.rows = out.len();
            Ok((out, meta))
        }
        _ => Err(ApiError::bad("the panel has no query")),
    }
}

async fn panel_data(
    s: &Shared,
    org: &Org,
    spec: &Value,
    panel_id: &str,
    dq: &DataQuery,
) -> Result<(Value, Meta), ApiError> {
    let panel =
        dash::find_panel(spec, panel_id).ok_or_else(|| ApiError::not_found("no such panel"))?;
    let ty = panel["type"].as_str().unwrap_or_default();
    let info = panels::info(ty).ok_or_else(|| ApiError::bad("unknown panel type"))?;
    if info.static_content {
        let content = panel["content"].as_str().unwrap_or_default();
        let html = if ty == "markdown" {
            panels::markdown_to_html(content)
        } else {
            panels::sanitize_html(content)
        };
        return Ok((
            json!({"type": ty, "html": html}),
            static_meta("dashboard_panel"),
        ));
    }
    let decls: Vec<Value> = spec["variables"].as_array().cloned().unwrap_or_default();
    let (rows, meta) =
        run_query(s, org, ty, &panel["query"], &decls, &dq.selected, dq.window).await?;
    let (mut columns, mut kinds) = panels::kinds(&rows);
    let mut rows = rows;
    // Pivot mode of a table panel: long rows become a grid with totals.
    if ty == "table"
        && let Some(pv) = panel["options"].get("pivot")
    {
        let g = |k: &str| pv[k].as_str().unwrap_or_default();
        let (cols, pivoted) = panels::pivot(&rows, g("row"), g("column"), g("value"));
        if pivoted.len() > 1 {
            rows = pivoted;
            columns = cols;
            kinds = panels::kinds(&rows).1;
        }
    }
    let shape_error = if rows.is_empty() {
        None
    } else {
        panels::check_shape(ty, &kinds).err()
    };
    let mut out = json!({
        "type": ty, "columns": columns, "rows": rows, "shape_error": shape_error,
        "truncated": rows.len() >= PANEL_ROWS, "from": dq.window.from_ms, "to": dq.window.to_ms,
    });
    // Only time series have a "same period earlier" to overlay.
    if let Some(c) = dq
        .compare
        .as_ref()
        .filter(|_| matches!(ty, "line" | "area" | "stacked_area" | "scatter"))
    {
        let (cw, off) = time::compare_window(dq.window, c)?;
        let (mut past, _) =
            run_query(s, org, ty, &panel["query"], &decls, &dq.selected, cw).await?;
        if info.shape == panels::Shape::Series
            && let Some(t) = out["columns"].get(0).and_then(Value::as_str)
        {
            time::shift_rows(&mut past, t, off);
        }
        out["compare"] =
            json!({"offset_ms": off, "from": cw.from_ms, "to": cw.to_ms, "rows": past});
    }
    Ok((out, meta))
}

pub async fn data(
    State(s): State<Shared>,
    Path((org, id, panel)): Path<(String, String, String)>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let dq = data_query(raw.as_deref(), s.now_ms())?;
    let row = load(&s, org, &id, DAY_KIND).await?;
    let spec: Value = serde_json::from_str(&row.spec).map_err(invalid)?;
    let (v, meta) = panel_data(&s, org, &spec, &panel, &dq).await?;
    Ok(ok(v, meta))
}

/// Values of every variable: custom ones from the spec, query ones by running their query.
async fn variable_options(
    s: &Shared,
    org: &Org,
    spec: &Value,
    dq: &DataQuery,
    show_errors: bool,
) -> Vec<Value> {
    let decls: Vec<Value> = spec["variables"].as_array().cloned().unwrap_or_default();
    let mut out = vec![];
    for d in &decls {
        let (mut options, mut error) = (vec![], None);
        if d["kind"] == "custom" {
            options = d["options"].as_array().cloned().unwrap_or_default();
        } else {
            let q = json!({"kind": "sql", "text": d["query"]});
            // A variable's own query may use earlier variables; one that cannot resolve is an error.
            match run_query(s, org, "table", &q, &decls, &dq.selected, dq.window).await {
                Ok((rows, _)) => {
                    for r in rows.iter().take(VARIABLE_VALUES * 2) {
                        if let Some(v) = r.as_object().and_then(|o| o.values().next()) {
                            let t = match v {
                                Value::String(s) => s.clone(),
                                Value::Null => continue,
                                other => other.to_string(),
                            };
                            if vars::valid_value(&t)
                                && !options.contains(&json!(t))
                                && options.len() < VARIABLE_VALUES
                            {
                                options.push(json!(t));
                            }
                        }
                    }
                }
                Err(e) => error = Some(e.1),
            }
        }
        let mut v = json!({"name": d["name"], "label": d["label"], "kind": d["kind"], "multi": d["multi"], "default": d["default"], "options": options});
        if let (Some(e), true) = (error, show_errors) {
            v["error"] = json!(e);
        }
        out.push(v);
    }
    out
}

pub async fn variables(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let dq = data_query(raw.as_deref(), s.now_ms())?;
    let row = load(&s, org, &id, DAY_KIND).await?;
    let spec: Value = serde_json::from_str(&row.spec).map_err(invalid)?;
    Ok(ok(
        variable_options(&s, org, &spec, &dq, true).await,
        static_meta("dashboard_variables"),
    ))
}

// ---- import, export, move ---------------------------------------------------------------

fn export_doc(d: &DashRow) -> Value {
    json!({dash::EXPORT_MARK: 1, "title": d.title, "spec": serde_json::from_str::<Value>(&d.spec).unwrap_or(Value::Null)})
}

pub async fn export_one(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let row = load(&s, org, &id, DAY_KIND).await?;
    Ok(ok(export_doc(&row), static_meta("dashboard_export")))
}

pub async fn export_all(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<ListParams>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let folder = p
        .folder
        .as_deref()
        .map(|f| hex_id(f, "folder"))
        .transpose()?;
    let (rows, meta) = s
        .read_store(
            build::dash_export_list(s.meta_db(), &org.name, folder.as_deref()).map_err(invalid)?,
        )
        .await?;
    let docs: Vec<Value> = rows.iter().map(|r| export_doc(&dash_row(r))).collect();
    Ok(ok(json!({dash::BUNDLE_MARK: 1, "dashboards": docs}), meta))
}

#[derive(Deserialize)]
pub struct ImportParams {
    folder: Option<String>,
}

fn import_one(doc: &Value, path: &str) -> Result<(String, Value), ApiError> {
    let o = doc
        .as_object()
        .ok_or_else(|| ApiError::bad(format!("{path}: must be an object")))?;
    if o.get(dash::EXPORT_MARK).and_then(Value::as_i64) != Some(1) {
        return Err(ApiError::bad(format!(
            "{path}: not a grepui dashboard export (missing \"{}\": 1)",
            dash::EXPORT_MARK
        )));
    }
    for k in o.keys() {
        if !["title", "spec", dash::EXPORT_MARK].contains(&k.as_str()) {
            return Err(ApiError::bad(format!("{path}: unknown key {k:?}")));
        }
    }
    let title = o
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad(format!("{path}.title: is required and must be a string")))?;
    let spec = dash::canonical(o.get("spec").unwrap_or(&Value::Null))
        .map_err(|e| ApiError::bad(e.1.replacen("spec", &format!("{path}.spec"), 1)))?;
    Ok((
        name(title, "title").map_err(|e| ApiError::bad(format!("{path}.title: {}", e.1)))?,
        spec,
    ))
}

/// Import one export document or a bundle. Each dashboard is a new dashboard (new id).
pub async fn import(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<ImportParams>,
    body: String,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let doc: Value = serde_json::from_str(&body).map_err(|e| {
        ApiError::bad(format!(
            "invalid JSON at line {}, column {}: {}",
            e.line(),
            e.column(),
            e
        ))
    })?;
    let items: Vec<(String, Value)> = if doc.get(dash::BUNDLE_MARK).is_some() {
        let arr = doc["dashboards"]
            .as_array()
            .ok_or_else(|| ApiError::bad("dashboards: must be a list"))?;
        if arr.is_empty() || arr.len() > 50 {
            return Err(ApiError::bad("dashboards: needs 1 to 50 dashboards"));
        }
        arr.iter()
            .enumerate()
            .map(|(i, d)| import_one(d, &format!("dashboards[{i}]")))
            .collect::<Result<_, _>>()?
    } else {
        vec![import_one(&doc, "")?]
    };
    let folder = check_folder(&s, org, p.folder.as_deref().unwrap_or_default()).await?;
    let now = s.now_ms();
    let mut created = vec![];
    let mut meta = static_meta("dash_put");
    for (title, spec) in items {
        let row = DashRow {
            dash_id: new_id(),
            folder_id: folder.clone(),
            title,
            kind: DAY_KIND.into(),
            spec: spec.to_string(),
            owner: LOCAL_USER.into(),
            created_ms: now,
            updated_ms: now,
            deleted: false,
        };
        meta = put(&s, org, &row).await?;
        created.push(json!({"id": row.dash_id, "title": row.title}));
    }
    Ok(status(
        StatusCode::CREATED,
        ok(json!({"imported": created}), meta),
    ))
}

#[derive(Deserialize)]
pub struct MoveBody {
    ids: Vec<String>,
    folder_id: String,
}

pub async fn move_many(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Json(b): Json<MoveBody>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    if b.ids.is_empty() || b.ids.len() > 100 {
        return Err(ApiError::bad("ids: needs 1 to 100 dashboards"));
    }
    let folder = check_folder(&s, org, &b.folder_id).await?;
    // Load everything first: a missing id fails the whole move before anything changes.
    let mut rows = vec![];
    for id in &b.ids {
        rows.push(load(&s, org, id, DAY_KIND).await?);
    }
    let now = s.now_ms();
    let mut meta = static_meta("dash_put");
    for r in &mut rows {
        r.folder_id = folder.clone();
        r.updated_ms = now;
        meta = put(&s, org, r).await?;
    }
    Ok(ok(json!({"moved": rows.len(), "folder_id": folder}), meta))
}

// ---- public links ---------------------------------------------------------------------------

fn token_hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn share_json(r: &ShareRow, now: i64) -> Value {
    json!({
        "id": r.share_id, "dashboard_id": r.dash_id, "created_ms": r.created_ms, "expires_ms": r.expires_ms,
        "revoked": r.revoked, "active": !r.revoked && r.expires_ms > now,
    })
}

fn share_row(r: &Value) -> ShareRow {
    ShareRow {
        share_id: r["share_id"].as_str().unwrap_or_default().into(),
        dash_id: r["dash_id"].as_str().unwrap_or_default().into(),
        token_hash: r["token_hash"].as_str().unwrap_or_default().into(),
        created_by: r["created_by"].as_str().unwrap_or(LOCAL_USER).into(),
        created_ms: r["created_ms"].as_i64().unwrap_or(0),
        expires_ms: r["expires_ms"].as_i64().unwrap_or(0),
        revoked: r["revoked"].as_bool().unwrap_or(false),
    }
}

pub async fn share_list(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let id = load(&s, org, &id, DAY_KIND).await?.dash_id;
    let (rows, meta) = s
        .read_store(build::share_list(s.meta_db(), &org.name, &id).map_err(invalid)?)
        .await?;
    let now = s.now_ms();
    Ok(ok(
        rows.iter()
            .map(|r| share_json(&share_row(r), now))
            .collect::<Vec<_>>(),
        meta,
    ))
}

#[derive(Deserialize)]
pub struct ShareBody {
    ttl_hours: i64,
}

/// Create a public link. The token is returned once; only its hash is stored.
pub async fn share_create(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
    Json(b): Json<ShareBody>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    if !(1..=MAX_SHARE_HOURS).contains(&b.ttl_hours) {
        return Err(ApiError::bad(format!(
            "ttl_hours must be 1 to {MAX_SHARE_HOURS}"
        )));
    }
    let dash = load(&s, org, &id, DAY_KIND).await?;
    let mut raw = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    let now = s.now_ms();
    let row = ShareRow {
        share_id: new_id(),
        dash_id: dash.dash_id,
        token_hash: token_hash(&token),
        created_by: LOCAL_USER.into(),
        created_ms: now,
        expires_ms: now + b.ttl_hours * 3_600_000,
        revoked: false,
    };
    let meta = s
        .write(build::share_put(s.meta_db(), &org.name, &row).map_err(invalid)?)
        .await?;
    let mut j = share_json(&row, now);
    j["token"] = json!(token);
    j["path"] = json!(format!("#/{}/public/{token}", org.name));
    Ok(status(StatusCode::CREATED, ok(j, meta)))
}

pub async fn share_revoke(
    State(s): State<Shared>,
    Path((org, id, share)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let dash_id = load(&s, org, &id, DAY_KIND).await?.dash_id;
    let (rows, _) = s
        .read_store(
            build::share_get(s.meta_db(), &org.name, &hex_id(&share, "share")?).map_err(invalid)?,
        )
        .await?;
    let mut row = share_row(
        rows.first()
            .ok_or_else(|| ApiError::not_found("no such link"))?,
    );
    if row.dash_id != dash_id {
        return Err(ApiError::not_found("no such link"));
    }
    row.revoked = true;
    let meta = s
        .write(build::share_put(s.meta_db(), &org.name, &row).map_err(invalid)?)
        .await?;
    Ok(ok(share_json(&row, s.now_ms()), meta))
}

/// A token resolves to its dashboard if the link is neither revoked nor expired.
async fn resolve(s: &Shared, org: &Org, token: &str) -> Result<DashRow, ApiError> {
    if token.len() != 32
        || !token
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(ApiError::not_found("no such link"));
    }
    let (rows, _) = s
        .read_store(
            build::share_by_hash(s.meta_db(), &org.name, &token_hash(token)).map_err(invalid)?,
        )
        .await?;
    let share = share_row(
        rows.first()
            .ok_or_else(|| ApiError::not_found("no such link"))?,
    );
    if share.revoked {
        return Err(ApiError::not_found("no such link"));
    }
    if share.expires_ms <= s.now_ms() {
        return Err(ApiError(StatusCode::GONE, "this link has expired".into()));
    }
    load(s, org, &share.dash_id, DAY_KIND).await
}

/// The dashboard as a public viewer sees it: title and spec without queries.
pub async fn public_get(
    State(s): State<Shared>,
    Path((org, token)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let row = resolve(&s, org, &token).await?;
    let spec: Value = serde_json::from_str(&row.spec).map_err(invalid)?;
    Ok(ok(
        json!({"title": row.title, "spec": dash::public_view(&spec), "read_only": true}),
        static_meta("public_dashboard"),
    ))
}

pub async fn public_variables(
    State(s): State<Shared>,
    Path((org, token)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let dq = data_query(raw.as_deref(), s.now_ms())?;
    let row = resolve(&s, org, &token).await?;
    let spec: Value = serde_json::from_str(&row.spec).map_err(invalid)?;
    Ok(ok(
        variable_options(&s, org, &spec, &dq, false).await,
        static_meta("dashboard_variables"),
    ))
}

pub async fn public_data(
    State(s): State<Shared>,
    Path((org, token, panel)): Path<(String, String, String)>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let dq = data_query(raw.as_deref(), s.now_ms())?;
    let row = resolve(&s, org, &token).await?;
    let spec: Value = serde_json::from_str(&row.spec).map_err(invalid)?;
    let (v, meta) = panel_data(&s, org, &spec, &panel, &dq).await?;
    Ok(ok(v, meta))
}

// ---- playlists ------------------------------------------------------------------------------

pub async fn playlist_list(
    State(s): State<Shared>,
    Path(org): Path<String>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let (rows, meta) = s
        .read_store(
            build::dash_list(s.meta_db(), &org.name, None, Some(PLAYLIST_KIND)).map_err(invalid)?,
        )
        .await?;
    Ok(ok(list_json(&rows), meta))
}

#[derive(Deserialize)]
pub struct PlaylistBody {
    title: Option<String>,
    spec: Option<Value>,
}

pub async fn playlist_create(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Json(b): Json<PlaylistBody>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let spec = dash::playlist_canonical(b.spec.as_ref().unwrap_or(&Value::Null))?;
    let now = s.now_ms();
    let row = DashRow {
        dash_id: new_id(),
        folder_id: String::new(),
        title: name(b.title.as_deref().unwrap_or_default(), "title")?,
        kind: PLAYLIST_KIND.into(),
        spec: spec.to_string(),
        owner: LOCAL_USER.into(),
        created_ms: now,
        updated_ms: now,
        deleted: false,
    };
    let meta = put(&s, org, &row).await?;
    Ok(status(StatusCode::CREATED, ok(dash_json(&row), meta)))
}

pub async fn playlist_get(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let row = load(&s, org, &id, PLAYLIST_KIND).await?;
    Ok(ok(dash_json(&row), static_meta("playlist_get")))
}

pub async fn playlist_update(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
    Json(b): Json<PlaylistBody>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let mut row = load(&s, org, &id, PLAYLIST_KIND).await?;
    if let Some(t) = &b.title {
        row.title = name(t, "title")?;
    }
    if let Some(sp) = &b.spec {
        row.spec = dash::playlist_canonical(sp)?.to_string();
    }
    row.updated_ms = s.now_ms();
    let meta = put(&s, org, &row).await?;
    Ok(ok(dash_json(&row), meta))
}

pub async fn playlist_delete(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let mut row = load(&s, org, &id, PLAYLIST_KIND).await?;
    row.deleted = true;
    row.updated_ms = s.now_ms();
    let meta = put(&s, org, &row).await?;
    Ok(ok(json!({"deleted": true, "id": row.dash_id}), meta))
}

/// Which dashboard a wall display should show now. It depends only on the clock, so displays
/// that ask at the same moment agree; `next_in_ms` says when to ask again.
pub async fn playlist_now(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let row = load(&s, org, &id, PLAYLIST_KIND).await?;
    let spec: Value = serde_json::from_str(&row.spec).map_err(invalid)?;
    let ids: Vec<String> = spec["dashboards"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| d.as_str().map(str::to_string))
        .collect();
    let (index, next_in_ms) = time::rotation(
        ids.len(),
        spec["interval_s"]
            .as_i64()
            .unwrap_or(time::PLAYLIST_DEFAULT_S),
        s.now_ms(),
    );
    Ok(ok(
        json!({"title": row.title, "index": index, "dashboard_id": ids.get(index), "next_in_ms": next_in_ms, "dashboards": ids, "interval_s": spec["interval_s"]}),
        static_meta("playlist_now"),
    ))
}

#[allow(dead_code)]
fn _unused(_: Bind) {}
