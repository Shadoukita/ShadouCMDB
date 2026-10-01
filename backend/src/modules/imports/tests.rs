//! Bulk import through the real router on a scratch database: upload,
//! limits, analysis by a worker, jobs, template, cleanup (SHAA-799 part 3).

use std::time::Duration;

use axum::Router;
use axum::body::Body as HttpBody;
use axum::http::{HeaderMap, Request, header};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::watch;
use tower::ServiceExt;
use uuid::Uuid;

use super::parse::fixtures::{C, Part, sheet_xml, workbook, workbook_parts, zip};
use super::upload::{CSV_TYPE, XLSX_TYPE};
use super::worker;
use crate::config::ImportConfig;
use crate::db::scratch;
use crate::modules::api_tokens::tests::{Creds, app_with_imports, call, code, session_of};

struct Env {
    app: Router,
    pool: PgPool,
    admin: Creds,
}

async fn env(pool: &PgPool, cfg: ImportConfig) -> Env {
    let app = app_with_imports(pool.clone(), cfg);
    let setup = json!({ "username": "admin", "displayName": "Admin", "password": "correct horse battery",
        "setupToken": crate::auth::setup_token::TEST_TOKEN });
    let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
    assert_eq!(status, 201, "{me}");
    let admin = session_of(&me, &headers);
    let (status, v, _) = call(&app, "PUT", "/api/v1/imports/settings", &admin, Some(json!({ "enabled": true }))).await;
    assert_eq!(status, 200, "{v}");
    Env { app, pool: pool.clone(), admin }
}

/// A user holding a profile with these global rights and full rights on every class.
async fn user(e: &Env, name: &str, global: &[&str]) -> Creds {
    user_in(e, name, global, None).await
}

/// A user with these global rights and full rights on `classes` only (every class for `None`).
async fn user_in(e: &Env, name: &str, global: &[&str], classes: Option<&[&str]>) -> Creds {
    let profile: Uuid = sqlx::query_scalar("INSERT INTO permission_profiles (name) VALUES ($1) RETURNING id")
        .bind(format!("{name} profile"))
        .fetch_one(&e.pool)
        .await
        .unwrap();
    for g in global {
        sqlx::query("INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES ($1, $2)")
            .bind(profile)
            .bind(g)
            .execute(&e.pool)
            .await
            .unwrap();
    }
    let class_ids: Vec<Option<Uuid>> = match classes {
        None => vec![None],
        Some(ids) => ids.iter().map(|c| Some(c.parse().unwrap())).collect(),
    };
    for class_id in class_ids {
        sqlx::query(
            "INSERT INTO permission_profile_class_permissions (profile_id, class_id, can_view, can_create, can_edit, can_delete)
             VALUES ($1, $2, true, true, true, true)",
        )
        .bind(profile)
        .bind(class_id)
        .execute(&e.pool)
        .await
        .unwrap();
    }
    let body = json!({ "username": name, "displayName": name, "password": "a long enough password",
        "profileIds": [profile] });
    let (status, v, _) = call(&e.app, "POST", "/api/v1/admin/users", &e.admin, Some(body)).await;
    assert_eq!(status, 201, "{v}");
    let login = json!({ "username": name, "password": "a long enough password" });
    let (status, me, headers) = call(&e.app, "POST", "/api/v1/auth/login", &Creds::default(), Some(login)).await;
    assert_eq!(status, 200, "{me}");
    session_of(&me, &headers)
}

/// A raw-body request.
async fn upload(
    app: &Router,
    creds: &Creds,
    content_type: &str,
    name: Option<&str>,
    extra: &[(&str, &str)],
    body: Vec<u8>,
) -> (u16, Value, HeaderMap) {
    let mut req = Request::builder().method("POST").uri("/api/v1/imports").header(header::CONTENT_TYPE, content_type);
    if let Some(c) = &creds.cookie {
        req = req.header(header::COOKIE, c);
    }
    if let Some(c) = &creds.csrf {
        req = req.header("x-csrf-token", c);
    }
    if let Some(n) = name {
        req = req.header("x-file-name", n);
    }
    for (k, v) in extra {
        req = req.header(*k, *v);
    }
    let res = app.clone().oneshot(req.body(HttpBody::from(body)).unwrap()).await.unwrap();
    let status = res.status().as_u16();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 22).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into())), headers)
}

fn detail(v: &Value) -> &str {
    v["error"]["details"][0]["code"].as_str().unwrap_or_default()
}

/// Runs queued phases until none is left (one worker, this process).
async fn drain(pool: &PgPool) {
    let cfg = std::sync::Arc::new(ImportConfig::default());
    let (_stop, mut rx) = watch::channel(false);
    while let Some(lease) = worker::claim(pool, "test-worker").await.unwrap() {
        worker::work(pool, &cfg, lease, &mut rx).await;
    }
}

async fn job(e: &Env, creds: &Creds, id: &str) -> Value {
    let (status, v, _) = call(&e.app, "GET", &format!("/api/v1/imports/{id}"), creds, None).await;
    assert_eq!(status, 200, "{v}");
    v
}

const CSV: &[u8] = "Hostname;Cores;Owner\nweb01;8;Müller\nweb02;16;\n\ndb01;4;Ops\n".as_bytes();

#[tokio::test(flavor = "multi_thread")]
async fn a_csv_upload_is_stored_analysed_and_described() {
    let Some(db) = scratch::database("import_csv_upload_analysed").await else { return };
    let e = env(&db.pool, ImportConfig::default()).await;
    let (status, v, headers) =
        upload(&e.app, &e.admin, CSV_TYPE, Some("servers%20%C3%BC.csv"), &[], CSV.to_vec()).await;
    assert_eq!(status, 202, "{v}");
    let id = v["id"].as_str().unwrap().to_owned();
    assert_eq!(headers.get(header::LOCATION).unwrap(), &format!("/api/v1/imports/{id}"));
    assert_eq!((v["status"].as_str(), v["phase"].as_str()), (Some("queued"), Some("analyse")));
    assert_eq!(v["file"]["name"], "servers ü.csv");
    assert_eq!(v["file"]["size"], CSV.len());
    assert_eq!(v["file"]["sha256"].as_str().map(str::len), Some(64));

    drain(&e.pool).await;
    let j = job(&e, &e.admin, &id).await;
    assert_eq!(j["status"], "ready", "{j}");
    assert_eq!((j["file"]["encoding"].as_str(), j["file"]["delimiter"].as_str()), (Some("utf-8"), Some(";")));
    assert_eq!((j["file"]["rowCount"].as_u64(), j["file"]["columnCount"].as_u64()), (Some(3), Some(3)));
    assert_eq!(j["columns"][0], json!({ "index": 0, "header": "Hostname", "samples": ["web01", "web02", "db01"] }));
    assert_eq!(j["columns"][2]["samples"], json!(["Müller", "Ops"]));
    assert_eq!(j["file"]["previewRows"][2], json!({ "row": 5, "cells": ["db01", "4", "Ops"] }));

    // Without a header row, columns are named by letter.
    let (status, v, _) = call(
        &e.app,
        "PATCH",
        &format!("/api/v1/imports/{id}/file-options"),
        &e.admin,
        Some(json!({ "hasHeaderRow": false })),
    )
    .await;
    assert_eq!((status, v["status"].as_str()), (202, Some("queued")), "{v}");
    drain(&e.pool).await;
    let j = job(&e, &e.admin, &id).await;
    assert_eq!((j["columns"][1]["header"].as_str(), j["file"]["rowCount"].as_u64()), (Some("Column B"), Some(4)));
    // Encoding and delimiter are for CSV, sheets for workbooks.
    let (status, v, _) = call(
        &e.app,
        "PATCH",
        &format!("/api/v1/imports/{id}/file-options"),
        &e.admin,
        Some(json!({ "sheet": "Servers" })),
    )
    .await;
    assert_eq!((status, v["error"]["details"][0]["field"].as_str()), (400, Some("sheet")));

    // Windows-1252 is detected when the file is not UTF-8.
    let latin = b"Name;Owner\nweb01;M\xfcller\n".to_vec();
    let (status, v, _) = upload(&e.app, &e.admin, CSV_TYPE, Some("latin.csv"), &[], latin).await;
    assert_eq!(status, 202, "{v}");
    drain(&e.pool).await;
    let j = job(&e, &e.admin, v["id"].as_str().unwrap()).await;
    assert_eq!(
        (j["file"]["encoding"].as_str(), j["columns"][1]["samples"][0].as_str()),
        (Some("windows-1252"), Some("Müller"))
    );
    db.drop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_xlsx_upload_lists_sheets_and_a_bad_one_fails_its_analysis() {
    let Some(db) = scratch::database("import_xlsx_upload").await else { return };
    let e = env(&db.pool, ImportConfig::default()).await;
    let parts = workbook_parts(
        &sheet_xml(&[("A1", C::S("Hostname")), ("A2", C::S("web01")), ("B2", C::N("12345"))]),
        Some(&sheet_xml(&[("A1", C::S("Note")), ("A2", C::S("n"))])),
        false,
        true,
    );
    let (status, v, _) = upload(&e.app, &e.admin, XLSX_TYPE, Some("a.xlsx"), &[], zip(&parts)).await;
    assert_eq!(status, 202, "{v}");
    let id = v["id"].as_str().unwrap().to_owned();
    drain(&e.pool).await;
    let j = job(&e, &e.admin, &id).await;
    assert_eq!(j["status"], "ready", "{j}");
    assert_eq!(
        (j["file"]["sheets"].clone(), j["file"]["hiddenSheets"].clone()),
        (json!(["Servers", "Notes"]), json!(["Notes"]))
    );
    assert_eq!(j["file"]["sheet"], "Servers");
    assert_eq!(j["columns"][1]["samples"], json!(["12345"]), "shortest decimal form");
    let (status, _, _) = call(
        &e.app,
        "PATCH",
        &format!("/api/v1/imports/{id}/file-options"),
        &e.admin,
        Some(json!({ "sheet": "Notes" })),
    )
    .await;
    assert_eq!(status, 202);
    drain(&e.pool).await;
    assert_eq!(job(&e, &e.admin, &id).await["columns"][0]["header"], "Note");

    // A zip bomb uploads (the bytes are small) and fails its analysis with the reason.
    let mut bomb = workbook_parts(&sheet_xml(&[]), None, false, false);
    bomb.push(Part::new("xl/media/bomb.bin", vec![0u8; 20 * 1024 * 1024]));
    let (status, v, _) = upload(&e.app, &e.admin, XLSX_TYPE, Some("bomb.xlsx"), &[], zip(&bomb)).await;
    assert_eq!(status, 202, "{v}");
    let id = v["id"].as_str().unwrap().to_owned();
    drain(&e.pool).await;
    let j = job(&e, &e.admin, &id).await;
    assert_eq!((j["status"].as_str(), j["error"]["code"].as_str()), (Some("failed"), Some("zip_bomb")), "{j}");
    // A failed analysis can be retried with other options; a failed job holds no import slot.
    let (status, _, _) = call(
        &e.app,
        "PATCH",
        &format!("/api/v1/imports/{id}/file-options"),
        &e.admin,
        Some(json!({ "hasHeaderRow": false })),
    )
    .await;
    assert_eq!(status, 202);
    db.drop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn uploads_are_refused_at_their_limits() {
    let Some(db) = scratch::database("import_upload_limits").await else { return };
    let cfg = ImportConfig { max_file_bytes: 2 * 1024 * 1024, max_stored_bytes: 3 * 1024 * 1024, ..Default::default() };
    let e = env(&db.pool, cfg).await;
    let up =
        |ct: &'static str, name: Option<&'static str>, body: Vec<u8>| upload(&e.app, &e.admin, ct, name, &[], body);
    let finish = |id: String| {
        let pool = e.pool.clone();
        async move {
            // Out of the way of the per-user limits: a finished job.
            sqlx::query("UPDATE cmdb.import_jobs SET status = 'completed', phase = NULL WHERE id = $1::uuid")
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
        }
    };

    // Content types: only the two file types; the CORS-safelisted ones get 415.
    for ct in ["text/plain", "multipart/form-data; boundary=x", "application/x-www-form-urlencoded", "application/json"]
    {
        let (status, v, _) = upload(&e.app, &e.admin, ct, Some("a.csv"), &[], CSV.to_vec()).await;
        assert_eq!((status, code(&v)), (415, "UNSUPPORTED_MEDIA_TYPE"), "{ct}");
    }
    // Magic bytes decide the format.
    let (status, v, _) = up(XLSX_TYPE, Some("x.xlsx"), vec![0xD0, 0xCF, 0x11, 0xE0, 1, 2, 3, 4]).await;
    assert_eq!((status, detail(&v)), (415, "workbook_encrypted_or_xls"));
    let (status, v, _) = up(CSV_TYPE, Some("x.csv"), workbook(&[("A1", C::S("x"))])).await;
    assert_eq!((status, detail(&v)), (415, "unsupported_format"));
    let (status, v, _) = up(XLSX_TYPE, Some("x.xlsx"), CSV.to_vec()).await;
    assert_eq!((status, detail(&v)), (415, "unsupported_format"));
    // The file name header.
    for (name, field_code) in [
        (None, "required"),
        (Some("a%2Fb.csv"), "invalid_character"),
        (Some("a%0Ab.csv"), "invalid_character"),
        // U+202E RIGHT-TO-LEFT OVERRIDE and U+200B ZERO WIDTH SPACE.
        (Some("report%E2%80%AEvsc.xlsx"), "invalid_character"),
        (Some("a%E2%80%8Bb.csv"), "invalid_character"),
        (Some("%FF.csv"), "invalid_format"),
    ] {
        let (status, v, _) = up(CSV_TYPE, name, CSV.to_vec()).await;
        assert_eq!(
            (status, v["error"]["details"][0]["field"].as_str(), detail(&v)),
            (400, Some("X-File-Name"), field_code),
            "{name:?}"
        );
    }
    let long = "a".repeat(256);
    let (status, _, _) = upload(&e.app, &e.admin, CSV_TYPE, Some(&long), &[], CSV.to_vec()).await;
    assert_eq!(status, 400);
    let (status, v, _) = up(CSV_TYPE, Some("empty.csv"), Vec::new()).await;
    assert_eq!((status, detail(&v)), (400, "empty_file"));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM cmdb.import_jobs").fetch_one(&e.pool).await.unwrap(),
        0,
        "refused uploads leave nothing behind"
    );

    // File size: exactly the limit is accepted, one byte more is 413 (with and without Content-Length).
    let limit = 2 * 1024 * 1024;
    let mut exact = b"a\n".repeat(limit / 2);
    exact.truncate(limit);
    let (status, v, _) = up(CSV_TYPE, Some("exact.csv"), exact.clone()).await;
    assert_eq!(status, 202, "{v}");
    finish(v["id"].as_str().unwrap().to_owned()).await;
    let mut over = exact.clone();
    over.push(b'a');
    let (status, v, _) = up(CSV_TYPE, Some("over.csv"), over).await;
    assert_eq!((status, code(&v)), (413, "PAYLOAD_TOO_LARGE"));

    // Stored bytes of the instance: 2 MiB stored, the cap is 3 MiB.
    let (status, v, _) = up(CSV_TYPE, Some("fits.csv"), exact[..1024 * 1024].to_vec()).await;
    assert_eq!(status, 202, "{v}");
    finish(v["id"].as_str().unwrap().to_owned()).await;
    let (status, v, _) = up(CSV_TYPE, Some("full.csv"), b"a\n".to_vec()).await;
    assert_eq!((status, detail(&v)), (429, "import_storage_full"), "{v}");
    sqlx::query("DELETE FROM cmdb.import_jobs").execute(&e.pool).await.unwrap();

    // One running job per user.
    let (status, v, _) = up(CSV_TYPE, Some("a.csv"), CSV.to_vec()).await;
    assert_eq!(status, 202);
    let first = v["id"].as_str().unwrap().to_owned();
    let (status, v, headers) = up(CSV_TYPE, Some("b.csv"), CSV.to_vec()).await;
    assert_eq!((status, detail(&v)), (429, "import_busy"));
    assert!(headers.contains_key(header::RETRY_AFTER));
    finish(first).await;

    // 20 unfinished jobs; the 21st is refused.
    sqlx::query("DELETE FROM cmdb.import_jobs").execute(&e.pool).await.unwrap();
    let admin_id: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE username = 'admin'").fetch_one(&e.pool).await.unwrap();
    let add = |status: &'static str, n: i64, ago: &'static str| {
        let pool = e.pool.clone();
        async move {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO cmdb.import_jobs (created_by_id, created_by_name, status, file_name, file_format, file_size,
                   file_sha256, expires_at, created_at)
                 SELECT $1, 'admin', '{status}', 'x.csv', 'csv', 1, repeat('0', 64), now() + interval '1 day', now() - interval '{ago}'
                 FROM generate_series(1, $2)"
            )))
            .bind(admin_id)
            .bind(n)
            .execute(&pool)
            .await
            .unwrap();
        }
    };
    add("ready", 19, "2 hours").await;
    let (status, v, _) = up(CSV_TYPE, Some("20th.csv"), CSV.to_vec()).await;
    assert_eq!(status, 202, "{v}");
    sqlx::query("UPDATE cmdb.import_jobs SET status = 'ready', phase = NULL WHERE file_name = '20th.csv'")
        .execute(&e.pool)
        .await
        .unwrap();
    let (status, v, _) = up(CSV_TYPE, Some("21st.csv"), CSV.to_vec()).await;
    assert_eq!((status, detail(&v)), (429, "import_limit"));

    // 30 uploads an hour; the 31st is refused.
    sqlx::query("DELETE FROM cmdb.import_jobs").execute(&e.pool).await.unwrap();
    add("completed", 29, "10 minutes").await;
    let (status, v, _) = up(CSV_TYPE, Some("30th.csv"), CSV.to_vec()).await;
    assert_eq!(status, 202, "{v}");
    finish(v["id"].as_str().unwrap().to_owned()).await;
    let (status, v, _) = up(CSV_TYPE, Some("31st.csv"), CSV.to_vec()).await;
    assert_eq!((status, detail(&v)), (429, "import_rate"));
    db.drop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn idempotency_keys_return_the_same_job() {
    let Some(db) = scratch::database("import_idempotency").await else { return };
    let e = env(&db.pool, ImportConfig::default()).await;
    let key = [("idempotency-key", "upload-1")];
    let (status, first, _) = upload(&e.app, &e.admin, CSV_TYPE, Some("a.csv"), &key, CSV.to_vec()).await;
    assert_eq!(status, 202, "{first}");
    // Sent again (say, after a lost answer): the same job, not a second one and not import_busy.
    let (status, again, _) = upload(&e.app, &e.admin, CSV_TYPE, Some("a.csv"), &key, CSV.to_vec()).await;
    assert_eq!((status, &again["id"]), (202, &first["id"]), "{again}");
    // The same key for another operation.
    sqlx::query("UPDATE cmdb.import_idempotency_keys SET operation = 'commit'").execute(&e.pool).await.unwrap();
    let (status, v, _) = upload(&e.app, &e.admin, CSV_TYPE, Some("a.csv"), &key, CSV.to_vec()).await;
    assert_eq!((status, code(&v), detail(&v)), (422, "IDEMPOTENCY_KEY_REUSED", "idempotency_key_reused"));
    let (status, v, _) =
        upload(&e.app, &e.admin, CSV_TYPE, Some("a.csv"), &[("idempotency-key", "bad key")], CSV.to_vec()).await;
    assert_eq!((status, v["error"]["details"][0]["field"].as_str()), (400, Some("Idempotency-Key")));
    db.drop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn only_owners_and_administrators_see_a_job_and_the_switch_gates_the_rest() {
    let Some(db) = scratch::database("import_job_access").await else { return };
    let e = env(&db.pool, ImportConfig::default()).await;
    let alice = user(&e, "alice", &["cis.import"]).await;
    let bob = user(&e, "bob", &["cis.import"]).await;
    let carol = user(&e, "carol", &[]).await;

    let (status, v, _) = upload(&e.app, &alice, CSV_TYPE, Some("a.csv"), &[], CSV.to_vec()).await;
    assert_eq!(status, 202, "{v}");
    let id = v["id"].as_str().unwrap().to_owned();
    drain(&e.pool).await;
    let path = format!("/api/v1/imports/{id}");

    // No cis.import: 403 everywhere.
    for (m, p) in [("GET", "/api/v1/imports".to_owned()), ("GET", path.clone()), ("DELETE", path.clone())] {
        let (status, v, _) = call(&e.app, m, &p, &carol, None).await;
        assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{m} {p}");
    }
    let (status, _, _) = upload(&e.app, &carol, CSV_TYPE, Some("c.csv"), &[], CSV.to_vec()).await;
    assert_eq!(status, 403);
    // Another user's job does not exist for bob; the administrator sees it.
    let (status, bob_view, _) = call(&e.app, "GET", &path, &bob, None).await;
    let other = Uuid::new_v4().to_string();
    let (_, missing, _) = call(&e.app, "GET", &format!("/api/v1/imports/{other}"), &bob, None).await;
    assert_eq!(status, 404);
    let message = |v: &Value, id: &str| v["error"]["message"].as_str().unwrap_or_default().replace(id, "<id>");
    assert_eq!(message(&bob_view, &id), message(&missing, &other), "the same answer as for a job that does not exist");
    for (m, p) in [("POST", format!("{path}/cancel")), ("DELETE", path.clone())] {
        assert_eq!(call(&e.app, m, &p, &bob, None).await.0, 404, "{m} {p}");
    }
    assert_eq!(job(&e, &e.admin, &id).await["createdBy"]["name"], "alice");
    let (_, list, _) = call(&e.app, "GET", "/api/v1/imports", &bob, None).await;
    assert_eq!(list["page"]["total"], 0);
    let (status, _, _) = call(&e.app, "GET", "/api/v1/imports?all=true", &bob, None).await;
    assert_eq!(status, 403);
    let (_, list, _) = call(&e.app, "GET", "/api/v1/imports?all=true", &e.admin, None).await;
    assert_eq!(list["page"]["total"], 1);
    let (_, list, _) = call(&e.app, "GET", "/api/v1/imports?status=ready", &alice, None).await;
    assert_eq!((list["page"]["total"].as_i64(), list["data"][0]["fileName"].as_str()), (Some(1), Some("a.csv")));

    // Switch off (W3): uploads and file options are refused, reading, cancelling and deleting still work.
    let (status, _, _) =
        call(&e.app, "PUT", "/api/v1/imports/settings", &e.admin, Some(json!({ "enabled": false }))).await;
    assert_eq!(status, 200);
    let (status, v, _) = upload(&e.app, &alice, CSV_TYPE, Some("b.csv"), &[], CSV.to_vec()).await;
    assert_eq!((status, detail(&v)), (403, "import_disabled"));
    let (status, v, _) =
        call(&e.app, "PATCH", &format!("{path}/file-options"), &alice, Some(json!({ "hasHeaderRow": true }))).await;
    assert_eq!((status, detail(&v)), (403, "import_disabled"));
    assert_eq!(call(&e.app, "GET", &path, &alice, None).await.0, 200);
    let (status, v, _) = call(&e.app, "POST", &format!("{path}/cancel"), &alice, None).await;
    assert_eq!((status, v["status"].as_str()), (202, Some("cancelled")));
    let (status, v, _) = call(&e.app, "POST", &format!("{path}/cancel"), &alice, None).await;
    assert_eq!((status, detail(&v)), (409, "invalid_state"));
    assert_eq!(call(&e.app, "DELETE", &path, &alice, None).await.0, 204);
    assert_eq!(call(&e.app, "GET", &path, &alice, None).await.0, 404);
    db.drop().await;
}

/// §5.5: deleting a user deletes their unfinished jobs and files; ended jobs stay, unowned.
#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_user_removes_their_unfinished_imports() {
    let Some(db) = scratch::database("import_user_deleted").await else { return };
    let e = env(&db.pool, ImportConfig::default()).await;
    let alice = user(&e, "alice", &["cis.import"]).await;
    let (_, first, _) = upload(&e.app, &alice, CSV_TYPE, Some("a.csv"), &[], CSV.to_vec()).await;
    drain(&e.pool).await;
    sqlx::query("UPDATE cmdb.import_jobs SET status = 'completed' WHERE id = $1::uuid")
        .bind(first["id"].as_str().unwrap())
        .execute(&e.pool)
        .await
        .unwrap();
    let (status, v, _) = upload(&e.app, &alice, CSV_TYPE, Some("b.csv"), &[], CSV.to_vec()).await;
    assert_eq!(status, 202, "{v}");
    drain(&e.pool).await;
    let alice_id: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE username = 'alice'").fetch_one(&e.pool).await.unwrap();
    let (status, _, _) = call(&e.app, "DELETE", &format!("/api/v1/admin/users/{alice_id}"), &e.admin, None).await;
    assert_eq!(status, 204);
    let left: Vec<(String, Option<Uuid>, String)> =
        sqlx::query_as("SELECT file_name, created_by_id, created_by_name FROM cmdb.import_jobs")
            .fetch_all(&e.pool)
            .await
            .unwrap();
    assert_eq!(left, [("a.csv".to_owned(), None, "alice".to_owned())]);
    let files: i64 = sqlx::query_scalar("SELECT count(*) FROM cmdb.import_job_files").fetch_one(&e.pool).await.unwrap();
    assert_eq!(files, 1, "only the ended job's file, until it expires");
    db.drop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_template_names_the_columns_and_neutralises_labels() {
    let Some(db) = scratch::database("import_template").await else { return };
    let e = env(&db.pool, ImportConfig::default()).await;
    crate::seed::install_template(&e.pool, "it_infrastructure").await.unwrap();
    sqlx::query(
        "UPDATE ci_attribute_definitions SET label = '=HYPERLINK(\"x\")'
         WHERE key = 'hostname' AND class_id = (SELECT id FROM ci_classes WHERE key = 'hardware')",
    )
    .execute(&e.pool)
    .await
    .unwrap();
    let req = Request::builder()
        .uri("/api/v1/imports/template?classKey=server")
        .header(header::COOKIE, e.admin.cookie.clone().unwrap())
        .body(HttpBody::empty())
        .unwrap();
    let res = e.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()[header::CONTENT_TYPE], "text/csv; charset=utf-8");
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    let disposition = res.headers()[header::CONTENT_DISPOSITION].to_str().unwrap().to_owned();
    assert!(disposition.starts_with("attachment; filename=\"server-template.csv\""), "{disposition}");
    let body = String::from_utf8(axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap().to_vec()).unwrap();
    assert!(body.starts_with("\u{feff}\"Ident\",\"Name\","), "{body}");
    assert!(body.contains("\"'=HYPERLINK(\"\"x\"\")\""), "{body}");
    assert!(body.ends_with("\r\n") && body.lines().count() == 1);
    // Abstract, unknown and forbidden classes look the same.
    for key in ["hardware", "nope"] {
        let (status, _, _) =
            call(&e.app, "GET", &format!("/api/v1/imports/template?classKey={key}"), &e.admin, None).await;
        assert_eq!(status, 404, "{key}");
    }
    db.drop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn workers_fence_their_writes_and_give_up_after_three_claims() {
    let Some(db) = scratch::database("import_worker_leases").await else { return };
    let pool = &db.pool;
    let insert = || async {
        sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO cmdb.import_jobs (created_by_name, status, phase, file_name, file_format, file_size, file_sha256,
               queued_at, expires_at)
             VALUES ('t', 'queued', 'analyse', 'a.csv', 'csv', 3, repeat('0', 64), now(), now() + interval '1 day')
             RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap()
    };
    let (a, b) = (insert().await, insert().await);
    // Two workers claim different jobs (SKIP LOCKED), never the same one.
    let (la, lb) = tokio::join!(worker::claim(pool, "A"), worker::claim(pool, "B"));
    let (la, lb) = (la.unwrap().unwrap(), lb.unwrap().unwrap());
    assert_ne!(la.job, lb.job);
    assert!([a, b].contains(&la.job) && [a, b].contains(&lb.job));
    assert!(worker::claim(pool, "C").await.unwrap().is_none(), "both are leased");

    // A's lease runs out; C takes the job over with a new epoch; A's writes are refused.
    sqlx::query("UPDATE cmdb.import_jobs SET lease_until = now() - interval '1 second' WHERE id = $1")
        .bind(la.job)
        .execute(pool)
        .await
        .unwrap();
    let lc = worker::claim(pool, "C").await.unwrap().unwrap();
    assert_eq!((lc.job, lc.epoch, lc.attempts), (la.job, la.epoch + 1, 2));
    assert!(!worker::write_progress(pool, &la, 10).await.unwrap(), "A was fenced off");
    assert!(!la.renew(pool).await.unwrap());
    assert!(!worker::fail(pool, &la, "x", "y").await.unwrap());
    assert!(worker::write_progress(pool, &lc, 10).await.unwrap());

    // A third claim without finishing fails the job for good (CR1).
    sqlx::query("UPDATE cmdb.import_jobs SET lease_until = now() - interval '1 second' WHERE id = $1")
        .bind(lc.job)
        .execute(pool)
        .await
        .unwrap();
    let third = worker::claim(pool, "D").await.unwrap().unwrap();
    assert_eq!(third.attempts, 3);
    let cfg = std::sync::Arc::new(ImportConfig::default());
    let (_stop, mut rx) = watch::channel(false);
    worker::work(pool, &cfg, third, &mut rx).await;
    let (status, code): (String, Option<String>) =
        sqlx::query_as("SELECT status, error ->> 'code' FROM cmdb.import_jobs WHERE id = $1")
            .bind(lc.job)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!((status.as_str(), code.as_deref()), ("failed", Some("internal_error")));
    assert!(worker::claim(pool, "E").await.unwrap().is_none_or(|l| l.job != lc.job), "not claimed again");
    db.drop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cleanup_expires_files_and_removes_stale_uploads() {
    let Some(db) = scratch::database("import_cleanup").await else { return };
    let e = env(&db.pool, ImportConfig::default()).await;
    let (_, v, _) = upload(&e.app, &e.admin, CSV_TYPE, Some("a.csv"), &[], CSV.to_vec()).await;
    let id: Uuid = v["id"].as_str().unwrap().parse().unwrap();
    drain(&e.pool).await;
    sqlx::query(
        "INSERT INTO cmdb.import_job_issues (job_id, seq, row_no, severity, code, message, phase, value)
         VALUES ($1, 0, 2, 'error', 'required', 'Name is required', 'validate', 'web01')",
    )
    .bind(id)
    .execute(&e.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO cmdb.import_jobs (created_by_name, status, file_name, file_format, file_size, expires_at, updated_at)
         VALUES ('t', 'uploading', 's.csv', 'csv', 0, now() + interval '1 day', now() - interval '2 hours')",
    )
    .execute(&e.pool)
    .await
    .unwrap();

    // Nothing is due yet, except the upload that stalled an hour ago.
    let c = worker::cleanup(&e.pool, chrono::Duration::zero()).await.unwrap();
    assert_eq!((c.stale_uploads, c.files_of_jobs, c.expired_jobs), (1, 0, 0));
    // 25 hours later: the file and the issues are gone, the job is expired, its counts stay.
    let c = worker::cleanup(&e.pool, chrono::Duration::hours(25)).await.unwrap();
    assert_eq!((c.files_of_jobs, c.expired_jobs), (1, 1));
    let j = job(&e, &e.admin, &id.to_string()).await;
    assert_eq!((j["status"].as_str(), j["file"]["rowCount"].as_u64()), (Some("expired"), Some(3)));
    let issues: i64 =
        sqlx::query_scalar("SELECT count(*) FROM cmdb.import_job_issues").fetch_one(&e.pool).await.unwrap();
    assert_eq!(issues, 0);
    // 90 days after it ended (it ended at the 25-hour mark), the record goes too.
    let c = worker::cleanup(&e.pool, chrono::Duration::days(92)).await.unwrap();
    assert_eq!(c.old_records, 1);
    db.drop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_upload_holds_no_connection_between_chunks() {
    let Some(db) = scratch::database("import_slow_upload").await else { return };
    let e = env(&db.pool, ImportConfig::default()).await;
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(1);
    let body = HttpBody::from_stream(tokio_stream_from(rx));
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/imports")
        .header(header::CONTENT_TYPE, CSV_TYPE)
        .header(header::COOKIE, e.admin.cookie.clone().unwrap())
        .header("x-csrf-token", e.admin.csrf.clone().unwrap())
        .header("x-file-name", "slow.csv")
        .body(body)
        .unwrap();
    let app = e.app.clone();
    let request = tokio::spawn(async move { app.oneshot(req).await.unwrap() });
    for b in b"Name\nweb01\n" {
        tx.send(Ok(axum::body::Bytes::copy_from_slice(&[*b]))).await.unwrap();
        // Between two bytes the upload holds no pool connection. sqlx returns
        // a released connection from a spawned task after a ping, so wait for
        // the pool to settle: a connection held across the wait never does.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while e.pool.size() as usize - e.pool.num_idle() > 0 {
            assert!(tokio::time::Instant::now() < deadline, "a connection is held while waiting for the client");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    drop(tx);
    let res = request.await.unwrap();
    assert_eq!(res.status(), 202);
    db.drop().await;
}

/// A byte stream from a channel (a client that sends slowly).
fn tokio_stream_from(
    mut rx: tokio::sync::mpsc::Receiver<Result<axum::body::Bytes, std::io::Error>>,
) -> impl futures_util::Stream<Item = Result<axum::body::Bytes, std::io::Error>> {
    futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx))
}

// ---------------------------------------------------------------------------
// Mapping and dry run (SHAA-799 part 4)
// ---------------------------------------------------------------------------

/// A class `srv` with a required text `hostname` (the title) and an integer `cores`.
async fn server_class(e: &Env) -> String {
    let (status, class, _) =
        call(&e.app, "POST", "/api/v1/ci-classes", &e.admin, Some(json!({ "key": "srv", "name": "Server" }))).await;
    assert_eq!(status, 201, "{class}");
    let class_id = class["id"].as_str().unwrap().to_owned();
    let mut host_id = String::new();
    for (key, label, t, required) in [("hostname", "Hostname", "text", true), ("cores", "Cores", "integer", false)] {
        let body = json!({ "classId": class_id, "key": key, "label": label, "dataType": t, "isRequired": required });
        let (status, v, _) = call(&e.app, "POST", "/api/v1/attribute-definitions", &e.admin, Some(body)).await;
        assert_eq!(status, 201, "{v}");
        if key == "hostname" {
            host_id = v["id"].as_str().unwrap().to_owned();
        }
    }
    let (status, v, _) = call(
        &e.app,
        "PATCH",
        &format!("/api/v1/ci-classes/{class_id}"),
        &e.admin,
        Some(json!({ "titleAttributeId": host_id })),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    class_id
}

async fn server(e: &Env, class: &str, host: &str, cores: i64) -> String {
    let body = json!({ "classId": class, "attributes": { "hostname": host, "cores": cores } });
    let (status, v, _) = call(&e.app, "POST", "/api/v1/configuration-items", &e.admin, Some(body)).await;
    assert_eq!(status, 201, "{v}");
    v["id"].as_str().unwrap().to_owned()
}

fn server_mapping() -> Value {
    json!({
        "classKey": "srv",
        "mode": "create_or_update",
        "key": { "field": "attributes.hostname" },
        "columns": [
            { "index": 0, "target": { "kind": "attribute", "key": "hostname" } },
            { "index": 1, "target": { "kind": "attribute", "key": "cores" } }
        ]
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dry_run_plans_every_row_and_writes_nothing() {
    let Some(db) = scratch::database("import_dry_run_plans_rows").await else { return };
    let e = env(&db.pool, ImportConfig::default()).await;
    let class = server_class(&e).await;
    let web01 = server(&e, &class, "web01", 8).await;
    server(&e, &class, "ops01", 2).await;

    let file = "Hostname;Cores\nweb01;12\nops01;2\nweb02;16\ndb01;many\nWEB03;4\nweb03 ;2\n";
    let (status, v, _) = upload(&e.app, &e.admin, CSV_TYPE, Some("srv.csv"), &[], file.as_bytes().to_vec()).await;
    assert_eq!(status, 202, "{v}");
    let id = v["id"].as_str().unwrap().to_owned();
    drain(&e.pool).await;
    assert_eq!(job(&e, &e.admin, &id).await["status"], "ready");

    // No mapping yet: the dry run is refused.
    let dry = format!("/api/v1/imports/{id}/dry-run");
    let (status, v, _) = call(&e.app, "POST", &dry, &e.admin, None).await;
    assert_eq!((status, detail(&v)), (409, "mapping_required"), "{v}");

    // Problems in the mapping come back all at once, with their field.
    let put = format!("/api/v1/imports/{id}/mapping");
    let mut bad = server_mapping();
    bad["key"] = json!({ "field": "attributes.nope" });
    bad["columns"][1]["target"]["key"] = json!("nope");
    let (status, v, _) = call(&e.app, "PUT", &put, &e.admin, Some(bad)).await;
    assert_eq!(status, 400, "{v}");
    let fields: Vec<&str> =
        v["error"]["details"].as_array().unwrap().iter().filter_map(|d| d["field"].as_str()).collect();
    assert!(fields.contains(&"columns[1].target.key"), "{v}");

    let (status, v, _) = call(&e.app, "PUT", &put, &e.admin, Some(server_mapping())).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!((v["status"].as_str(), v["mapping"]["classKey"].as_str()), (Some("ready"), Some("srv")));

    let (status, v, _) = call(&e.app, "POST", &dry, &e.admin, None).await;
    assert_eq!(status, 202, "{v}");
    assert_eq!((v["status"].as_str(), v["phase"].as_str()), (Some("queued"), Some("validate")));
    let cis_before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM configuration_items").fetch_one(&e.pool).await.unwrap();
    drain(&e.pool).await;

    let j = job(&e, &e.admin, &id).await;
    assert_eq!(j["status"], "validated", "{j}");
    let s = &j["summary"];
    assert_eq!(
        (s["create"].as_u64(), s["update"].as_u64(), s["unchanged"].as_u64(), s["errorRows"].as_u64()),
        (Some(1), Some(1), Some(1), Some(3)),
        "{j}"
    );
    assert_eq!(j["dryRun"]["stale"], false);
    let update = j["preview"].as_array().unwrap().iter().find(|p| p["outcome"] == "update").unwrap();
    assert_eq!((update["row"].as_u64(), update["ciId"].as_str()), (Some(2), Some(web01.as_str())));
    assert_eq!(update["changes"], json!([{ "field": "attributes.cores", "old": 8, "new": 12 }]));
    let created = j["preview"].as_array().unwrap().iter().find(|p| p["outcome"] == "create").unwrap();
    assert_eq!(created["ciLabel"], "web02");

    // Read-only: nothing was written.
    let cis_after: i64 =
        sqlx::query_scalar("SELECT count(*) FROM configuration_items").fetch_one(&e.pool).await.unwrap();
    assert_eq!(cis_after, cis_before);
    let (_, ci, _) = call(&e.app, "GET", &format!("/api/v1/configuration-items/{web01}"), &e.admin, None).await;
    assert_eq!(ci["attributes"]["cores"], 8);

    let issues = format!("/api/v1/imports/{id}/issues");
    let (status, v, _) = call(&e.app, "GET", &format!("{issues}?severity=error"), &e.admin, None).await;
    assert_eq!(status, 200, "{v}");
    let got: Vec<(u64, &str)> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| (i["row"].as_u64().unwrap(), i["code"].as_str().unwrap()))
        .collect();
    assert_eq!(got, vec![(5, "invalid_type"), (6, "duplicate_key_in_file"), (7, "duplicate_key_in_file")], "{v}");
    let cores = &v["data"][0];
    assert_eq!(
        (cores["column"].as_u64(), cores["header"].as_str(), cores["value"].as_str()),
        (Some(1), Some("Cores"), Some("many"))
    );
    let (_, v, _) = call(&e.app, "GET", &format!("{issues}?code=invalid_type&limit=1"), &e.admin, None).await;
    assert_eq!(v["page"]["total"], 1, "{v}");

    // A new mapping drops the dry run and its problems.
    let (status, v, _) = call(&e.app, "PUT", &put, &e.admin, Some(server_mapping())).await;
    assert_eq!((status, v["summary"].is_null()), (200, true), "{v}");
    let (_, v, _) = call(&e.app, "GET", &issues, &e.admin, None).await;
    assert_eq!(v["page"]["total"], 0, "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dry_run_stops_when_the_owner_loses_the_import_right() {
    let Some(db) = scratch::database("import_dry_run_owner_revoked").await else { return };
    let e = env(&db.pool, ImportConfig::default()).await;
    server_class(&e).await;
    let alice = user(&e, "alice", &["cis.import"]).await;
    let (status, v, _) =
        upload(&e.app, &alice, CSV_TYPE, Some("a.csv"), &[], b"Hostname;Cores\nweb09;1\n".to_vec()).await;
    assert_eq!(status, 202, "{v}");
    let id = v["id"].as_str().unwrap().to_owned();
    drain(&e.pool).await;
    let (status, v, _) =
        call(&e.app, "PUT", &format!("/api/v1/imports/{id}/mapping"), &alice, Some(server_mapping())).await;
    assert_eq!(status, 200, "{v}");
    let (status, v, _) = call(&e.app, "POST", &format!("/api/v1/imports/{id}/dry-run"), &alice, None).await;
    assert_eq!(status, 202, "{v}");
    sqlx::query("DELETE FROM permission_profile_global_permissions WHERE permission = 'cis.import'")
        .execute(&e.pool)
        .await
        .unwrap();
    drain(&e.pool).await;
    let j = job(&e, &e.admin, &id).await;
    assert_eq!(
        (j["status"].as_str(), j["error"]["code"].as_str()),
        (Some("failed"), Some("permission_revoked")),
        "{j}"
    );
}

// ---------------------------------------------------------------------------
// Commit (SHAA-799 part 4, §2.6)
// ---------------------------------------------------------------------------

/// Uploads `file`, maps it with [`server_mapping`] and runs the dry run.
async fn validated(e: &Env, creds: &Creds, file: &str) -> String {
    let (status, v, _) = upload(&e.app, creds, CSV_TYPE, Some("srv.csv"), &[], file.as_bytes().to_vec()).await;
    assert_eq!(status, 202, "{v}");
    let id = v["id"].as_str().unwrap().to_owned();
    drain(&e.pool).await;
    let (status, v, _) =
        call(&e.app, "PUT", &format!("/api/v1/imports/{id}/mapping"), creds, Some(server_mapping())).await;
    assert_eq!(status, 200, "{v}");
    let (status, v, _) = call(&e.app, "POST", &format!("/api/v1/imports/{id}/dry-run"), creds, None).await;
    assert_eq!(status, 202, "{v}");
    drain(&e.pool).await;
    assert_eq!(job(e, creds, &id).await["status"], "validated");
    id
}

/// `POST /imports/{id}/commit`, with an optional `Idempotency-Key`.
async fn commit(e: &Env, creds: &Creds, id: &str, skip: bool, key: Option<&str>) -> (u16, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/imports/{id}/commit"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = &creds.cookie {
        req = req.header(header::COOKIE, c);
    }
    if let Some(c) = &creds.csrf {
        req = req.header("x-csrf-token", c);
    }
    if let Some(k) = key {
        req = req.header("idempotency-key", k);
    }
    let body = json!({ "skipErrorRows": skip }).to_string();
    let res = e.app.clone().oneshot(req.body(HttpBody::from(body)).unwrap()).await.unwrap();
    let status = res.status().as_u16();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 22).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn committed(j: &Value) -> (u64, u64, u64, u64, u64) {
    let c = &j["summary"]["committed"];
    let n = |k: &str| c[k].as_u64().unwrap_or(u64::MAX);
    (n("created"), n("updated"), n("unchanged"), n("skipped"), n("failed"))
}

async fn count(pool: &PgPool, sql: &str, job: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned())).bind(format!("import:{job}")).fetch_one(pool).await.unwrap()
}

/// The type table of `srv`, for triggers that fail rows only at commit.
async fn srv_table(pool: &PgPool) -> String {
    sqlx::query_scalar(
        "SELECT format('%I.%I', table_schema, table_name) FROM information_schema.columns
         WHERE table_name = 'srv' AND column_name = 'hostname'",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

fn hosts(n: usize) -> String {
    let mut file = String::from("Hostname;Cores\n");
    for i in 0..n {
        file.push_str(&format!("host-{i:05};{}\n", i % 64));
    }
    file
}

#[tokio::test(flavor = "multi_thread")]
async fn a_commit_writes_the_valid_rows_and_audits_them() {
    let Some(db) = scratch::database("import_commit_writes").await else { return };
    let e = env(&db.pool, ImportConfig::default()).await;
    let class = server_class(&e).await;
    let web01 = server(&e, &class, "web01", 8).await;
    let ops01 = server(&e, &class, "ops01", 2).await;
    // A title longer than a label: the label is its first 500 characters, trimmed (T9).
    let long = format!("{} {}", "x".repeat(499), "y".repeat(100));
    let file = format!("Hostname;Cores\nweb01;12\nops01;2\nweb02;16\ndb01;many\n{long};1\n");
    let id = validated(&e, &e.admin, &file).await;
    let j = job(&e, &e.admin, &id).await;
    assert_eq!(j["summary"]["errorRows"], 1, "{j}");
    let planned_label = j["preview"].as_array().unwrap().iter().find(|p| p["row"] == 6).unwrap()["ciLabel"].clone();
    assert_eq!(planned_label, json!("x".repeat(499)));

    // Error rows need skipErrorRows.
    let (status, v) = commit(&e, &e.admin, &id, false, None).await;
    assert_eq!((status, detail(&v)), (409, "has_error_rows"), "{v}");
    let (status, v) = commit(&e, &e.admin, &id, true, Some("commit-1")).await;
    assert_eq!(status, 202, "{v}");
    assert_eq!((v["status"].as_str(), v["phase"].as_str()), (Some("queued"), Some("commit")));
    // The same key again: the job as it is, no second commit.
    let (status, v) = commit(&e, &e.admin, &id, true, Some("commit-1")).await;
    assert_eq!((status, v["status"].as_str()), (202, Some("queued")), "{v}");
    drain(&e.pool).await;

    let j = job(&e, &e.admin, &id).await;
    assert_eq!(j["status"], "completed_with_errors", "{j}");
    assert_eq!(committed(&j), (2, 1, 1, 1, 0), "{j}");
    let (_, ci, _) = call(&e.app, "GET", &format!("/api/v1/configuration-items/{web01}"), &e.admin, None).await;
    assert_eq!((ci["attributes"]["cores"].as_i64(), ci["version"].as_i64()), (Some(12), Some(2)), "{ci}");
    let label: String = sqlx::query_scalar("SELECT label FROM configuration_items WHERE label LIKE 'xxx%'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(json!(label), planned_label, "the dry run's label is the stored one (T9)");

    // Audit (§4.3): per CI as the owner via import, nothing for the unchanged row, one import.commit.
    let per_ci = count(
        &e.pool,
        "SELECT count(*) FROM audit_log WHERE request_id = $1 AND entity_type = 'configuration_items'
           AND actor_type = 'import' AND actor_name = 'admin'",
        &id,
    )
    .await;
    assert_eq!(per_ci, 3);
    let unchanged: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE entity_id = $1 AND request_id LIKE 'import:%'")
            .bind(Uuid::parse_str(&ops01).unwrap())
            .fetch_one(&e.pool)
            .await
            .unwrap();
    assert_eq!(unchanged, 0);
    let event: Value = sqlx::query_scalar(
        "SELECT new_value FROM audit_log WHERE action = 'import.commit' AND entity_type = 'import_jobs' AND entity_id = $1",
    )
    .bind(Uuid::parse_str(&id).unwrap())
    .fetch_one(&e.pool)
    .await
    .unwrap();
    assert_eq!(
        (event["outcome"].as_str(), event["created"].as_u64(), event["skipped"].as_u64(), event["classKey"].as_str()),
        (Some("completed_with_errors"), Some(2), Some(1), Some("srv")),
        "{event}"
    );

    // A job commits once.
    let (status, v) = commit(&e, &e.admin, &id, true, None).await;
    assert_eq!((status, detail(&v)), (409, "invalid_state"), "{v}");

    // The same file again: nothing changes and nothing is audited (D1).
    let again = validated(&e, &e.admin, &file).await;
    let (status, v) = commit(&e, &e.admin, &again, true, None).await;
    assert_eq!(status, 202, "{v}");
    drain(&e.pool).await;
    let j = job(&e, &e.admin, &again).await;
    assert_eq!(committed(&j), (0, 0, 4, 1, 0), "{j}");
    let per_ci = count(
        &e.pool,
        "SELECT count(*) FROM audit_log WHERE request_id = $1 AND entity_type = 'configuration_items'",
        &again,
    )
    .await;
    assert_eq!(per_ci, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_commit_needs_a_current_dry_run() {
    let Some(db) = scratch::database("import_commit_stale").await else { return };
    let e = env(&db.pool, ImportConfig::default()).await;
    let class = server_class(&e).await;
    let (status, v, _) = upload(&e.app, &e.admin, CSV_TYPE, Some("s.csv"), &[], hosts(3).into_bytes()).await;
    assert_eq!(status, 202, "{v}");
    let id = v["id"].as_str().unwrap().to_owned();
    drain(&e.pool).await;
    let (status, v, _) =
        call(&e.app, "PUT", &format!("/api/v1/imports/{id}/mapping"), &e.admin, Some(server_mapping())).await;
    assert_eq!(status, 200, "{v}");
    let (status, v) = commit(&e, &e.admin, &id, false, None).await;
    assert_eq!((status, detail(&v)), (409, "dry_run_required"), "{v}");

    let id = validated(&e, &e.admin, &hosts(3)).await;
    // The data model changes after the dry run (T14).
    let body = json!({ "classId": class, "key": "rack", "label": "Rack", "dataType": "text" });
    let (status, v, _) = call(&e.app, "POST", "/api/v1/attribute-definitions", &e.admin, Some(body)).await;
    assert_eq!(status, 201, "{v}");
    let j = job(&e, &e.admin, &id).await;
    assert_eq!(
        (j["dryRun"]["stale"].as_bool(), j["dryRun"]["staleReason"].as_str()),
        (Some(true), Some("model_changed"))
    );
    let (status, v) = commit(&e, &e.admin, &id, false, None).await;
    assert_eq!((status, detail(&v)), (409, "dry_run_stale"), "{v}");

    // A dry run older than 24 hours.
    let id = validated(&e, &e.admin, &hosts(3)).await;
    sqlx::query("UPDATE import_jobs SET dry_run_finished_at = now() - interval '25 hours' WHERE id = $1")
        .bind(Uuid::parse_str(&id).unwrap())
        .execute(&e.pool)
        .await
        .unwrap();
    let (status, v) = commit(&e, &e.admin, &id, false, None).await;
    assert_eq!((status, detail(&v)), (409, "dry_run_stale"), "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_row_failing_only_at_commit_is_failed_and_the_other_499_are_written() {
    let Some(db) = scratch::database("import_commit_replay").await else { return };
    let e = env(&db.pool, ImportConfig::default()).await;
    server_class(&e).await;
    let table = srv_table(&e.pool).await;
    // A rule the plan step does not know: only the database refuses the row (T2).
    for sql in [
        "CREATE FUNCTION public.refuse_boom() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN IF NEW.hostname = 'host-00250' THEN
           RAISE EXCEPTION 'refused' USING ERRCODE = 'check_violation'; END IF; RETURN NEW; END $$"
            .to_owned(),
        format!(
            "CREATE TRIGGER refuse_boom BEFORE INSERT ON {table} FOR EACH ROW EXECUTE FUNCTION public.refuse_boom()"
        ),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(sql)).execute(&e.pool).await.unwrap();
    }
    let id = validated(&e, &e.admin, &hosts(500)).await;
    assert_eq!(job(&e, &e.admin, &id).await["summary"]["create"], 500);
    let (status, v) = commit(&e, &e.admin, &id, false, None).await;
    assert_eq!(status, 202, "{v}");
    drain(&e.pool).await;

    let j = job(&e, &e.admin, &id).await;
    assert_eq!(j["status"], "completed_with_errors", "{j}");
    assert_eq!(committed(&j), (499, 0, 0, 0, 1), "{j}");
    let written: i64 = sqlx::query_scalar("SELECT count(*) FROM configuration_items").fetch_one(&e.pool).await.unwrap();
    assert_eq!(written, 499);
    let (_, v, _) = call(&e.app, "GET", &format!("/api/v1/imports/{id}/issues?severity=error"), &e.admin, None).await;
    let rows: Vec<u64> = v["data"].as_array().unwrap().iter().map(|i| i["row"].as_u64().unwrap()).collect();
    assert_eq!(rows, vec![252], "{v}");
    let per_ci = count(
        &e.pool,
        "SELECT count(*) FROM audit_log WHERE request_id = $1 AND entity_type = 'configuration_items'",
        &id,
    )
    .await;
    assert_eq!(per_ci, 499);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deadlock_runs_the_chunk_again_and_it_commits_once() {
    let Some(db) = scratch::database("import_commit_deadlock").await else { return };
    let e = env(&db.pool, ImportConfig::default()).await;
    server_class(&e).await;
    let table = srv_table(&e.pool).await;
    // The first insert of the commit loses a deadlock (T3); a sequence is not rolled back.
    for sql in [
        "CREATE SEQUENCE public.deadlock_once".to_owned(),
        "CREATE FUNCTION public.deadlock_once() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN IF nextval('public.deadlock_once') = 1 THEN
           RAISE EXCEPTION 'deadlock detected' USING ERRCODE = 'deadlock_detected'; END IF; RETURN NEW; END $$"
            .to_owned(),
        format!(
            "CREATE TRIGGER deadlock_once BEFORE INSERT ON {table} FOR EACH ROW EXECUTE FUNCTION public.deadlock_once()"
        ),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(sql)).execute(&e.pool).await.unwrap();
    }
    let id = validated(&e, &e.admin, &hosts(20)).await;
    let (status, v) = commit(&e, &e.admin, &id, false, None).await;
    assert_eq!(status, 202, "{v}");
    drain(&e.pool).await;
    let j = job(&e, &e.admin, &id).await;
    assert_eq!((j["status"].as_str(), committed(&j)), (Some("completed"), (20, 0, 0, 0, 0)), "{j}");
    let written: i64 = sqlx::query_scalar("SELECT count(*) FROM configuration_items").fetch_one(&e.pool).await.unwrap();
    assert_eq!(written, 20);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_killed_commit_resumes_after_its_cursor_and_a_stalled_worker_is_fenced() {
    let Some(db) = scratch::database("import_commit_resume").await else { return };
    let e = env(&db.pool, ImportConfig::default()).await;
    server_class(&e).await;

    // The worker dies after the first chunk; the next one resumes after the cursor.
    let id = validated(&e, &e.admin, &hosts(1_200)).await;
    let job_id = Uuid::parse_str(&id).unwrap();
    let (status, v) = commit(&e, &e.admin, &id, false, None).await;
    assert_eq!(status, 202, "{v}");
    super::commit::test_hooks::die_after(job_id, 1);
    drain(&e.pool).await;
    let (status, cursor): (String, i32) =
        sqlx::query_as("SELECT status, committed_through_row FROM import_jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(&e.pool)
            .await
            .unwrap();
    assert_eq!((status.as_str(), cursor), ("committing", 501));
    sqlx::query("UPDATE import_jobs SET lease_until = now() - interval '1 second' WHERE id = $1")
        .bind(job_id)
        .execute(&e.pool)
        .await
        .unwrap();
    drain(&e.pool).await;
    let j = job(&e, &e.admin, &id).await;
    assert_eq!((j["status"].as_str(), committed(&j)), (Some("completed"), (1_200, 0, 0, 0, 0)), "{j}");
    let (all, distinct): (i64, i64) = sqlx::query_as("SELECT count(*), count(DISTINCT label) FROM configuration_items")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!((all, distinct), (1_200, 1_200), "no row was written twice");
    let events =
        count(&e.pool, "SELECT count(*) FROM audit_log WHERE action = 'import.commit' AND $1 <> ''", &id).await;
    assert_eq!(events, 1);

    // Worker A stalls past its lease, B takes the job over: A's chunk is rolled back (T13).
    let file: String = hosts(1_800).lines().skip(1_201).map(|l| format!("{l}\n")).collect();
    let id = validated(&e, &e.admin, &format!("Hostname;Cores\n{file}")).await;
    let job_id = Uuid::parse_str(&id).unwrap();
    let (status, v) = commit(&e, &e.admin, &id, false, None).await;
    assert_eq!(status, 202, "{v}");
    let a = worker::claim(&e.pool, "worker-a").await.unwrap().unwrap();
    sqlx::query("UPDATE import_jobs SET lease_until = now() - interval '1 second' WHERE id = $1")
        .bind(job_id)
        .execute(&e.pool)
        .await
        .unwrap();
    let b = worker::claim(&e.pool, "worker-b").await.unwrap().unwrap();
    assert_eq!((b.job, b.epoch), (a.job, a.epoch + 1));
    let cfg = ImportConfig::default();
    let lost = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    super::commit::run(&e.pool, &cfg, &a, &lost).await;
    let written: i64 = sqlx::query_scalar("SELECT count(*) FROM configuration_items").fetch_one(&e.pool).await.unwrap();
    assert_eq!(written, 1_200, "A wrote nothing");
    let (_stop, mut rx) = watch::channel(false);
    worker::work(&e.pool, &std::sync::Arc::new(cfg), b, &mut rx).await;
    let j = job(&e, &e.admin, &id).await;
    assert_eq!((j["status"].as_str(), committed(&j)), (Some("completed"), (600, 0, 0, 0, 0)), "{j}");
    let written: i64 = sqlx::query_scalar("SELECT count(*) FROM configuration_items").fetch_one(&e.pool).await.unwrap();
    assert_eq!(written, 1_800);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_commit_stops_when_the_owner_loses_the_import_right_and_keeps_earlier_chunks() {
    let Some(db) = scratch::database("import_commit_revoked").await else { return };
    let e = env(&db.pool, ImportConfig::default()).await;
    server_class(&e).await;
    let alice = user(&e, "alice", &["cis.import"]).await;
    let id = validated(&e, &alice, &hosts(700)).await;
    let job_id = Uuid::parse_str(&id).unwrap();
    let (status, v) = commit(&e, &alice, &id, false, None).await;
    assert_eq!(status, 202, "{v}");
    super::commit::test_hooks::die_after(job_id, 1);
    drain(&e.pool).await;
    sqlx::query("DELETE FROM permission_profile_global_permissions WHERE permission = 'cis.import'")
        .execute(&e.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE import_jobs SET lease_until = now() - interval '1 second' WHERE id = $1")
        .bind(job_id)
        .execute(&e.pool)
        .await
        .unwrap();
    drain(&e.pool).await;
    let j = job(&e, &e.admin, &id).await;
    assert_eq!(
        (j["status"].as_str(), j["error"]["code"].as_str(), committed(&j)),
        (Some("failed"), Some("permission_revoked"), (500, 0, 0, 0, 0)),
        "{j}"
    );
    let written: i64 = sqlx::query_scalar("SELECT count(*) FROM configuration_items").fetch_one(&e.pool).await.unwrap();
    assert_eq!(written, 500);
    let event: Value = sqlx::query_scalar(
        "SELECT new_value FROM audit_log WHERE action = 'import.commit' AND entity_id = $1 AND actor_type = 'import'
           AND actor_name = 'alice'",
    )
    .bind(job_id)
    .fetch_one(&e.pool)
    .await
    .unwrap();
    assert_eq!((event["outcome"].as_str(), event["created"].as_u64()), (Some("failed"), Some(500)), "{event}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_commit_cancelled_during_a_chunk_ends_cancelled_with_its_final_counts() {
    let Some(db) = scratch::database("import_commit_cancel_running").await else { return };
    let e = env(&db.pool, ImportConfig::default()).await;
    server_class(&e).await;
    let id = validated(&e, &e.admin, &hosts(1_500)).await;
    let job_id = Uuid::parse_str(&id).unwrap();
    let (status, v) = commit(&e, &e.admin, &id, false, None).await;
    assert_eq!(status, 202, "{v}");

    // The stop arrives while the second chunk is being written (GH#359).
    let (reached, go) = super::commit::test_hooks::pause_at(job_id, 2);
    let pool = e.pool.clone();
    let worker = tokio::spawn(async move { drain(&pool).await });
    reached.await.unwrap();
    let (status, v, _) = call(&e.app, "POST", &format!("/api/v1/imports/{id}/cancel"), &e.admin, None).await;
    assert_eq!((status, v["status"].as_str()), (202, Some("committing")), "{v}");
    assert!(v["cancelRequestedAt"].is_string() && v["finishedAt"].is_null(), "{v}");
    // A second stop is accepted and changes nothing.
    let (status, again, _) = call(&e.app, "POST", &format!("/api/v1/imports/{id}/cancel"), &e.admin, None).await;
    assert_eq!((status, &again["cancelRequestedAt"]), (202, &v["cancelRequestedAt"]), "{again}");

    // Poll as the wizard does: the first `cancelled` it sees carries the final counts.
    go.send(()).unwrap();
    let first = loop {
        let j = job(&e, &e.admin, &id).await;
        if j["status"] != "committing" {
            break j;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    worker.await.unwrap();
    assert_eq!((first["status"].as_str(), committed(&first)), (Some("cancelled"), (1_000, 0, 0, 0, 0)), "{first}");
    assert_eq!(first["progress"]["done"], 1_000, "{first}");
    assert!(first["finishedAt"].is_string(), "{first}");
    let last = job(&e, &e.admin, &id).await;
    assert_eq!(
        (&last["summary"], &last["progress"], &last["finishedAt"]),
        (&first["summary"], &first["progress"], &first["finishedAt"])
    );
    let written: i64 = sqlx::query_scalar("SELECT count(*) FROM configuration_items").fetch_one(&e.pool).await.unwrap();
    assert_eq!(written, 1_000, "the chunk in flight is written, the third is not");
    let events: Vec<Value> =
        sqlx::query_scalar("SELECT new_value FROM audit_log WHERE action = 'import.commit' AND entity_id = $1")
            .bind(job_id)
            .fetch_all(&e.pool)
            .await
            .unwrap();
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!((events[0]["outcome"].as_str(), events[0]["created"].as_u64()), (Some("cancelled"), Some(1_000)));
    assert_eq!(events[0]["finishedAt"], first["finishedAt"]);
    let (status, v, _) = call(&e.app, "POST", &format!("/api/v1/imports/{id}/cancel"), &e.admin, None).await;
    assert_eq!((status, code(&v)), (409, "CONFLICT"), "{v}");

    // A stop requested while no worker runs the commit (it died) ends it when
    // the next worker takes the job over, before that worker writes anything.
    let id = validated(&e, &e.admin, &hosts(1_500).replace("host", "node")).await;
    let job_id = Uuid::parse_str(&id).unwrap();
    let (status, v) = commit(&e, &e.admin, &id, false, None).await;
    assert_eq!(status, 202, "{v}");
    super::commit::test_hooks::die_after(job_id, 1);
    drain(&e.pool).await;
    let (status, v, _) = call(&e.app, "POST", &format!("/api/v1/imports/{id}/cancel"), &e.admin, None).await;
    assert_eq!((status, v["status"].as_str()), (202, Some("committing")), "{v}");
    sqlx::query("UPDATE import_jobs SET lease_until = now() - interval '1 second' WHERE id = $1")
        .bind(job_id)
        .execute(&e.pool)
        .await
        .unwrap();
    drain(&e.pool).await;
    let j = job(&e, &e.admin, &id).await;
    assert_eq!((j["status"].as_str(), committed(&j)), (Some("cancelled"), (500, 0, 0, 0, 0)), "{j}");
    let written: i64 = sqlx::query_scalar("SELECT count(*) FROM configuration_items").fetch_one(&e.pool).await.unwrap();
    assert_eq!(written, 1_500);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_commit_cancelled_while_queued_writes_nothing_and_is_audited_once() {
    let Some(db) = scratch::database("import_commit_cancel_queued").await else { return };
    let e = env(&db.pool, ImportConfig::default()).await;
    server_class(&e).await;
    let id = validated(&e, &e.admin, &hosts(3)).await;
    let (status, v) = commit(&e, &e.admin, &id, false, None).await;
    assert_eq!((status, v["status"].as_str()), (202, Some("queued")), "{v}");
    let (status, v, _) = call(&e.app, "POST", &format!("/api/v1/imports/{id}/cancel"), &e.admin, None).await;
    assert_eq!((status, v["status"].as_str()), (202, Some("cancelled")), "{v}");
    drain(&e.pool).await;

    assert_eq!(job(&e, &e.admin, &id).await["status"], "cancelled");
    let written: i64 = sqlx::query_scalar("SELECT count(*) FROM configuration_items").fetch_one(&e.pool).await.unwrap();
    assert_eq!(written, 0);
    // No worker ever held it, so the cancel itself writes the one import.commit (§4.3).
    let events: Vec<Value> = sqlx::query_scalar(
        "SELECT new_value FROM audit_log WHERE action = 'import.commit' AND entity_id = $1
           AND actor_type = 'import' AND actor_name = 'admin' AND request_id = 'import:' || $1::text",
    )
    .bind(Uuid::parse_str(&id).unwrap())
    .fetch_all(&e.pool)
    .await
    .unwrap();
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!((events[0]["outcome"].as_str(), events[0]["created"].as_u64()), (Some("cancelled"), Some(0)));

    // A dry run that is cancelled is no commit: no event.
    let (status, v, _) = upload(&e.app, &e.admin, CSV_TYPE, Some("b.csv"), &[], hosts(2).into_bytes()).await;
    assert_eq!(status, 202, "{v}");
    let other = v["id"].as_str().unwrap().to_owned();
    drain(&e.pool).await;
    let (status, v, _) =
        call(&e.app, "PUT", &format!("/api/v1/imports/{other}/mapping"), &e.admin, Some(server_mapping())).await;
    assert_eq!(status, 200, "{v}");
    let (status, v, _) = call(&e.app, "POST", &format!("/api/v1/imports/{other}/dry-run"), &e.admin, None).await;
    assert_eq!(status, 202, "{v}");
    let (status, v, _) = call(&e.app, "POST", &format!("/api/v1/imports/{other}/cancel"), &e.admin, None).await;
    assert_eq!(status, 202, "{v}");
    let events = count(&e.pool, "SELECT count(*) FROM audit_log WHERE request_id = $1", &other).await;
    assert_eq!(events, 0);
}

/// `srv` with a reference attribute `peer` (to `srv`) and a `depends_on`
/// relationship type from `srv` to `srv`.
async fn linked_server_class(e: &Env) -> (String, Uuid) {
    let class = server_class(e).await;
    let body = json!({
        "classId": class, "key": "peer", "label": "Peer", "dataType": "reference", "referenceClassId": class
    });
    let (status, v, _) = call(&e.app, "POST", "/api/v1/attribute-definitions", &e.admin, Some(body)).await;
    assert_eq!(status, 201, "{v}");
    let depends: Uuid = sqlx::query_scalar(
        "INSERT INTO relationship_types (key, name, forward_label, reverse_label)
         VALUES ('depends_on', 'Depends on', 'depends on', 'required by') RETURNING id",
    )
    .fetch_one(&e.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO relationship_type_rules (relationship_type_id, source_class_id, target_class_id) VALUES ($1, $2, $2)",
    )
    .bind(depends)
    .bind(class.parse::<Uuid>().unwrap())
    .execute(&e.pool)
    .await
    .unwrap();
    (class, depends)
}

fn linked_mapping() -> Value {
    let by_host = json!({ "by": "attribute", "attributeKey": "hostname" });
    json!({
        "classKey": "srv",
        "mode": "create_or_update",
        "key": { "field": "attributes.hostname" },
        "columns": [
            { "index": 0, "target": { "kind": "attribute", "key": "hostname" } },
            { "index": 1, "target": { "kind": "attribute", "key": "cores" } },
            { "index": 2, "target": { "kind": "attribute", "key": "peer", "match": by_host } },
            { "index": 3, "target": {
                "kind": "relationship", "typeKey": "depends_on", "direction": "outgoing", "match": by_host
            } }
        ]
    })
}

async fn validated_linked(e: &Env, file: &str) -> String {
    let (status, v, _) = upload(&e.app, &e.admin, CSV_TYPE, Some("linked.csv"), &[], file.as_bytes().to_vec()).await;
    assert_eq!(status, 202, "{v}");
    let id = v["id"].as_str().unwrap().to_owned();
    drain(&e.pool).await;
    let (status, v, _) =
        call(&e.app, "PUT", &format!("/api/v1/imports/{id}/mapping"), &e.admin, Some(linked_mapping())).await;
    assert_eq!(status, 200, "{v}");
    let (status, v, _) = call(&e.app, "POST", &format!("/api/v1/imports/{id}/dry-run"), &e.admin, None).await;
    assert_eq!(status, 202, "{v}");
    drain(&e.pool).await;
    assert_eq!(job(e, &e.admin, &id).await["status"], "validated");
    id
}

async fn ci_id(pool: &PgPool, label: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM configuration_items WHERE label = $1").bind(label).fetch_one(pool).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn references_reach_cis_of_earlier_rows_and_relationships_are_only_added() {
    let Some(db) = scratch::database("import_commit_references").await else { return };
    let e = env(&db.pool, ImportConfig::default()).await;
    let (class, depends) = linked_server_class(&e).await;
    let web01 = server(&e, &class, "web01", 8).await;
    let db01 = server(&e, &class, "db01", 4).await;
    let edge = json!({ "relationshipTypeId": depends, "sourceCiId": web01, "targetCiId": db01 });
    let (status, v, _) = call(&e.app, "POST", "/api/v1/relationships", &e.admin, Some(edge)).await;
    assert_eq!(status, 201, "{v}");

    // Row 2 refers to CIs in the database; row 3 to the CI row 2 creates
    // (pending); row 4 leaves web01's existing relationship out; row 5 names
    // the CI of row 6, which is created later (forward).
    let file = "Hostname,Cores,Peer,Depends on\n\
                app01,4,web01,db01;web01;db01\n\
                app02,4,app01,app01\n\
                web01,8,,\n\
                app03,4,app04,\n\
                app04,4,,\n";
    let id = validated_linked(&e, file).await;
    let j = job(&e, &e.admin, &id).await;
    let s = &j["summary"];
    assert_eq!(
        (s["create"].as_u64(), s["unchanged"].as_u64(), s["errorRows"].as_u64()),
        (Some(3), Some(1), Some(1)),
        "{j}"
    );
    let (_, v, _) = call(&e.app, "GET", &format!("/api/v1/imports/{id}/issues?severity=error"), &e.admin, None).await;
    let got: Vec<(u64, &str, &str)> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| (i["row"].as_u64().unwrap(), i["code"].as_str().unwrap(), i["header"].as_str().unwrap()))
        .collect();
    assert_eq!(got, vec![(5, "reference_to_later_row", "Peer")], "{v}");

    let (status, v) = commit(&e, &e.admin, &id, true, None).await;
    assert_eq!(status, 202, "{v}");
    drain(&e.pool).await;
    let j = job(&e, &e.admin, &id).await;
    assert_eq!((j["status"].as_str(), committed(&j)), (Some("completed_with_errors"), (3, 0, 1, 1, 0)), "{j}");
    // app01 → db01, app01 → web01 (the second db01 in the cell is the same edge), app02 → app01.
    assert_eq!(j["summary"]["committed"]["relationshipsAdded"], 3, "{j}");

    let (app01, app02) = (ci_id(&e.pool, "app01").await, ci_id(&e.pool, "app02").await);
    let (_, ci, _) = call(&e.app, "GET", &format!("/api/v1/configuration-items/{app02}"), &e.admin, None).await;
    assert_eq!(ci["attributes"]["peer"], json!(app01.to_string()), "the pending CI of row 2: {ci}");
    let (_, ci, _) = call(&e.app, "GET", &format!("/api/v1/configuration-items/{app01}"), &e.admin, None).await;
    assert_eq!(ci["attributes"]["peer"], json!(web01), "{ci}");
    let skipped: i64 = sqlx::query_scalar("SELECT count(*) FROM configuration_items WHERE label = 'app03'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(skipped, 0);

    let mut edges: Vec<(String, String)> = sqlx::query_as(
        "SELECT s.label, t.label FROM ci_relationships r
           JOIN configuration_items s ON s.id = r.source_ci_id JOIN configuration_items t ON t.id = r.target_ci_id
          WHERE r.relationship_type_id = $1",
    )
    .bind(depends)
    .fetch_all(&e.pool)
    .await
    .unwrap();
    edges.sort();
    let pair = |a: &str, b: &str| (a.to_owned(), b.to_owned());
    assert_eq!(
        edges,
        vec![pair("app01", "db01"), pair("app01", "web01"), pair("app02", "app01"), pair("web01", "db01")],
        "web01's relationship was left out of the file and stays"
    );
    let audited = count(
        &e.pool,
        "SELECT count(*) FROM audit_log WHERE request_id = $1 AND entity_type = 'ci_relationships' AND actor_type = 'import'",
        &id,
    )
    .await;
    assert_eq!(audited, 3);

    // Again: every edge exists already, so nothing is added.
    let again = validated_linked(&e, "Hostname,Cores,Peer,Depends on\napp01,4,web01,db01;web01\n").await;
    let (status, v) = commit(&e, &e.admin, &again, false, None).await;
    assert_eq!(status, 202, "{v}");
    drain(&e.pool).await;
    let j = job(&e, &e.admin, &again).await;
    assert_eq!((committed(&j), &j["summary"]["committed"]["relationshipsAdded"]), ((0, 0, 1, 0, 0), &json!(0)), "{j}");
}

// ---------------------------------------------------------------------------
// Error report (SHAA-799 part 4, §3.4, §5.1)
// ---------------------------------------------------------------------------

/// A GET whose body is not JSON: status, headers and body text.
async fn download(e: &Env, creds: &Creds, uri: &str) -> (u16, HeaderMap, String) {
    let mut req = Request::builder().uri(uri);
    if let Some(c) = &creds.cookie {
        req = req.header(header::COOKIE, c);
    }
    let res = e.app.clone().oneshot(req.body(HttpBody::empty()).unwrap()).await.unwrap();
    let status = res.status().as_u16();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 22).await.unwrap();
    (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_error_report_lists_each_problem_with_its_row_and_audits_other_readers() {
    let Some(db) = scratch::database("import_error_report").await else { return };
    let e = env(&db.pool, ImportConfig::default()).await;
    server_class(&e).await;
    let alice = user(&e, "alice", &["cis.import"]).await;
    let bob = user(&e, "bob", &["cis.import"]).await;
    let id = validated(&e, &alice, "Hostname;Cores\nweb01;=1+1\nweb02;4\n\n@db01;many\n").await;
    let uri = format!("/api/v1/imports/{id}/error-report");

    let (status, headers, body) = download(&e, &alice, &uri).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(headers[header::CONTENT_TYPE], "text/csv; charset=utf-8");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let disposition = headers[header::CONTENT_DISPOSITION].to_str().unwrap();
    assert!(disposition.starts_with("attachment; filename=\"srv-errors.csv\""), "{disposition}");
    let lines: Vec<&str> = body.strip_prefix('\u{feff}').expect("a byte order mark").split_terminator("\r\n").collect();
    assert_eq!(lines[0], "\"Row\";\"Severity\";\"Column\";\"Problem\";\"Code\";\"Hostname\";\"Cores\"", "{body}");
    assert_eq!(lines.len(), 3, "one line per problem, none for the valid row: {body}");
    assert!(lines[1].starts_with("\"2\";\"error\";\"Cores\";") && lines[1].ends_with(";\"web01\";\"'=1+1\""), "{body}");
    assert!(lines[2].starts_with("\"5\";\"error\";\"Cores\";") && lines[2].ends_with(";\"'@db01\";\"many\""), "{body}");
    assert!(!body.contains("web02"), "{body}");
    let job_uuid = Uuid::parse_str(&id).unwrap();
    let reads = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_log WHERE action = 'import.report_read' AND entity_id = $1",
        )
        .bind(job_uuid)
        .fetch_one(&e.pool)
        .await
        .unwrap()
    };
    assert_eq!(reads().await, 0, "the owner's own download is not audited");

    // Someone else's job is not found; an administrator's download is audited (W7).
    let (status, _, _) = download(&e, &bob, &uri).await;
    assert_eq!(status, 404);
    let (status, _, admin_body) = download(&e, &e.admin, &uri).await;
    assert_eq!((status, admin_body == body), (200, true));
    assert_eq!(reads().await, 1);
    let event: Value = sqlx::query_scalar("SELECT new_value FROM audit_log WHERE action = 'import.report_read'")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!((event["fileName"].as_str(), event["ownerName"].as_str()), (Some("srv.csv"), Some("alice")), "{event}");

    // Uploaded again, the report reads as the original cells.
    let (status, v, _) = upload(&e.app, &alice, CSV_TYPE, Some("srv-errors.csv"), &[], body.into_bytes()).await;
    assert_eq!(status, 202, "{v}");
    let again = v["id"].as_str().unwrap().to_owned();
    drain(&e.pool).await;
    let j = job(&e, &alice, &again).await;
    let cells = &j["file"]["previewRows"][0]["cells"];
    assert_eq!((cells[5].as_str(), cells[6].as_str()), (Some("web01"), Some("=1+1")), "{j}");

    // A job without problems has no report.
    let clean = validated(&e, &alice, "Hostname;Cores\nweb09;1\n").await;
    let (status, _, _) = download(&e, &alice, &format!("/api/v1/imports/{clean}/error-report")).await;
    assert_eq!(status, 404);
    db.drop().await;
}

// ---------------------------------------------------------------------------
// Saved mappings and suggestions (SHAA-799 part 4, D9, §3.3)
// ---------------------------------------------------------------------------

/// A saved-mapping definition: `hostname` from "hostname", the other headers ignored.
fn definition(headers: &[&str], extra: Value) -> Value {
    let mut columns: Vec<Value> = headers
        .iter()
        .map(|h| {
            let target = if *h == "hostname" {
                json!({ "kind": "attribute", "key": "hostname" })
            } else {
                json!({ "kind": "ignore" })
            };
            json!({ "header": h, "target": target })
        })
        .collect();
    if let Value::Array(more) = extra {
        for m in more {
            let h = m["header"].clone();
            columns.retain(|c| c["header"] != h);
            columns.push(m);
        }
    }
    json!({ "mode": "create_or_update", "key": { "field": "attributes.hostname" }, "columns": columns })
}

async fn vm_class(e: &Env) -> String {
    let (status, class, _) =
        call(&e.app, "POST", "/api/v1/ci-classes", &e.admin, Some(json!({ "key": "vm", "name": "VM" }))).await;
    assert_eq!(status, 201, "{class}");
    class["id"].as_str().unwrap().to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn saved_mappings_are_shared_per_class_and_only_their_creator_changes_them() {
    let Some(db) = scratch::database("import_saved_mappings").await else { return };
    let e = env(&db.pool, ImportConfig::default()).await;
    let srv = server_class(&e).await;
    vm_class(&e).await;
    let alice = user(&e, "alice", &["cis.import"]).await;
    let bob = user_in(&e, "bob", &["cis.import"], Some(&[srv.as_str()])).await;
    let carol = user(&e, "carol", &[]).await;
    let def = definition(&["hostname", "Cores"], json!([]));
    let body = |name: &str, class: &str| json!({ "name": name, "classKey": class, "definition": def });

    let (status, v, _) = call(&e.app, "GET", "/api/v1/import-mappings", &carol, None).await;
    assert_eq!(status, 403, "{v}");
    let (status, m, _) =
        call(&e.app, "POST", "/api/v1/import-mappings", &alice, Some(body(" Vendor export ", "srv"))).await;
    assert_eq!(status, 201, "{m}");
    assert_eq!((m["name"].as_str(), m["version"].as_i64()), (Some("Vendor export"), Some(1)));
    assert_eq!((m["createdBy"]["name"].as_str(), m["classKey"].as_str()), (Some("alice"), Some("srv")));
    assert_eq!(
        m["definition"]["columns"][0],
        json!({ "header": "hostname", "target": { "kind": "attribute", "key": "hostname" } })
    );
    let srv_mapping = m["id"].as_str().unwrap().to_owned();

    // Names are unique per class, ignoring case (T18); the same name on another class is fine.
    let (status, v, _) =
        call(&e.app, "POST", "/api/v1/import-mappings", &bob, Some(body("VENDOR EXPORT", "srv"))).await;
    assert_eq!((status, detail(&v)), (409, "duplicate_name"), "{v}");
    let (status, v, _) =
        call(&e.app, "POST", "/api/v1/import-mappings", &alice, Some(body("Vendor export", "vm"))).await;
    assert_eq!(status, 201, "{v}");
    let vm_mapping = v["id"].as_str().unwrap().to_owned();

    // A class bob cannot view is refused like an unknown one, and its mappings are hidden.
    let (status, v, _) = call(&e.app, "POST", "/api/v1/import-mappings", &bob, Some(body("Mine", "vm"))).await;
    assert_eq!((status, detail(&v)), (400, "unknown_class"), "{v}");
    let (status, v, _) = call(&e.app, "POST", "/api/v1/import-mappings", &bob, Some(body("Mine", "nope"))).await;
    assert_eq!((status, detail(&v)), (400, "unknown_class"), "{v}");
    let (_, v, _) = call(&e.app, "GET", "/api/v1/import-mappings", &bob, None).await;
    let ids: Vec<&str> = v["data"].as_array().unwrap().iter().filter_map(|m| m["id"].as_str()).collect();
    assert_eq!(ids, [srv_mapping.as_str()], "{v}");
    let (status, _, _) = call(&e.app, "GET", &format!("/api/v1/import-mappings/{vm_mapping}"), &bob, None).await;
    assert_eq!(status, 404);
    let (_, v, _) = call(&e.app, "GET", "/api/v1/import-mappings?classKey=vm", &alice, None).await;
    assert_eq!(v["data"].as_array().unwrap().len(), 1, "{v}");

    // Definitions are checked for duplicate headers and size.
    let dup = definition(&["hostname", "Host Name", "host_name"], json!([]));
    let (status, v, _) = call(
        &e.app,
        "POST",
        "/api/v1/import-mappings",
        &alice,
        Some(json!({ "name": "Dup", "classKey": "srv", "definition": dup })),
    )
    .await;
    assert_eq!((status, detail(&v)), (400, "duplicate_header"), "{v}");
    let long: Vec<String> = (0..70).map(|i| format!("{i:03}{}", "x".repeat(997))).collect();
    let long: Vec<&str> = long.iter().map(String::as_str).collect();
    let (status, v, _) = call(
        &e.app,
        "POST",
        "/api/v1/import-mappings",
        &alice,
        Some(json!({ "name": "Big", "classKey": "srv", "definition": definition(&long, json!([])) })),
    )
    .await;
    assert_eq!((status, detail(&v)), (400, "too_large"), "{v}");

    // Only the creator (or an administrator) changes it, with the version they loaded.
    let one = format!("/api/v1/import-mappings/{srv_mapping}");
    let (status, v, _) = call(&e.app, "PATCH", &one, &bob, Some(json!({ "version": 1, "name": "Taken" }))).await;
    assert_eq!(status, 403, "{v}");
    let patch = json!({ "version": 1, "name": "Vendor", "description": "From the vendor portal" });
    let (status, v, _) = call(&e.app, "PATCH", &one, &alice, Some(patch.clone())).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!((v["version"].as_i64(), v["description"].as_str()), (Some(2), Some("From the vendor portal")));
    let (status, v, _) = call(&e.app, "PATCH", &one, &alice, Some(patch)).await;
    assert_eq!((status, v["error"]["code"].as_str()), (409, Some("VERSION_CONFLICT")), "{v}");
    let (status, v, _) =
        call(&e.app, "PATCH", &one, &e.admin, Some(json!({ "version": 2, "description": null }))).await;
    assert_eq!(
        (status, v["description"].is_null(), v["updatedBy"]["name"].as_str()),
        (200, true, Some("admin")),
        "{v}"
    );

    let (status, _, _) = call(&e.app, "DELETE", &format!("{one}?version=3"), &bob, None).await;
    assert_eq!(status, 403);
    let (status, v, _) = call(&e.app, "DELETE", &format!("{one}?version=1"), &alice, None).await;
    assert_eq!(status, 409, "{v}");
    let (status, _, _) = call(&e.app, "DELETE", &format!("{one}?version=3"), &alice, None).await;
    assert_eq!(status, 204);
    let (status, _, _) = call(&e.app, "GET", &one, &alice, None).await;
    assert_eq!(status, 404);

    // Every change is audited with its class key, for the audit visibility rule (T21).
    let audit: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT action::text, coalesce(new_value ->> 'classKey', old_value ->> 'classKey')
         FROM audit_log WHERE entity_type = 'import_mappings' ORDER BY id",
    )
    .fetch_all(&e.pool)
    .await
    .unwrap();
    let srv_key = Some("srv".to_owned());
    assert_eq!(
        audit,
        [
            ("create".into(), srv_key.clone()),
            ("create".into(), Some("vm".into())),
            ("update".into(), srv_key.clone()),
            ("update".into(), srv_key.clone()),
            ("delete".into(), srv_key)
        ]
    );

    // At most 500 per instance.
    sqlx::query(
        "INSERT INTO import_mappings (name, class_key, definition, created_by_name, updated_by_name)
         SELECT 'm' || n, 'srv', '{}', 'x', 'x' FROM generate_series(1, 499) n",
    )
    .execute(&e.pool)
    .await
    .unwrap();
    let (status, v, _) = call(&e.app, "POST", "/api/v1/import-mappings", &alice, Some(body("One more", "srv"))).await;
    assert_eq!((status, detail(&v)), (409, "limit_reached"), "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_suggestion_matches_headers_by_saved_mapping_key_and_label() {
    let Some(db) = scratch::database("import_mapping_suggestion").await else { return };
    let e = env(&db.pool, ImportConfig::default()).await;
    let srv = server_class(&e).await;
    for (key, label) in [("serial_no", "Serial number"), ("depends_note", "Depends on")] {
        let body = json!({ "classId": srv, "key": key, "label": label, "dataType": "text" });
        let (status, v, _) = call(&e.app, "POST", "/api/v1/attribute-definitions", &e.admin, Some(body)).await;
        assert_eq!(status, 201, "{v}");
    }
    for (key, forward, reverse) in [("runs_on", "Runs on", "Hosts"), ("depends_on", "Depends on", "Required by")] {
        let t: Uuid = sqlx::query_scalar(
            "INSERT INTO relationship_types (key, name, forward_label, reverse_label) VALUES ($1, $1, $2, $3) RETURNING id",
        )
        .bind(key)
        .bind(forward)
        .bind(reverse)
        .fetch_one(&e.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO relationship_type_rules (relationship_type_id, source_class_id, target_class_id) VALUES ($1, $2, $2)",
        )
        .bind(t)
        .bind(srv.parse::<Uuid>().unwrap())
        .execute(&e.pool)
        .await
        .unwrap();
    }
    vm_class(&e).await;
    let alice = user(&e, "alice", &["cis.import"]).await;

    let headers = ["hostname", "Serial-Number", "Runs on", "Hosts", "Depends on", "Ident", "Cores", "cores", "Notes"];
    let file = format!("{}\nweb01;S1;app01;;;;4;4;x\n", headers.join(";"));
    let (status, v, _) = upload(&e.app, &alice, CSV_TYPE, Some("srv.csv"), &[], file.into_bytes()).await;
    assert_eq!(status, 202, "{v}");
    let id = v["id"].as_str().unwrap().to_owned();
    drain(&e.pool).await;
    let suggest = format!("/api/v1/imports/{id}/mapping-suggestion?classKey=srv");
    let vias = |v: &Value| -> Vec<(Value, Value)> {
        v["matchedBy"].as_array().unwrap().iter().map(|m| (m["via"].clone(), m["hint"].clone())).collect()
    };
    let (key, label, saved, none) = (json!("key"), json!("label"), json!("saved_mapping"), Value::Null);

    // alice is no administrator: the ident column is not suggested for new CIs.
    let (status, v, _) = call(&e.app, "GET", &suggest, &alice, None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        vias(&v),
        [
            (key.clone(), none.clone()),
            (label.clone(), none.clone()),
            (label.clone(), none.clone()),
            (label.clone(), none.clone()),
            (none.clone(), json!("ambiguous_label")),
            (none.clone(), json!("ident_admin_only")),
            (key.clone(), none.clone()),
            (none.clone(), json!("duplicate_target")),
            (none.clone(), none.clone()),
        ],
        "{v}"
    );
    let m = &v["mapping"];
    assert_eq!(
        (m["mode"].as_str(), m["key"]["field"].as_str()),
        (Some("create_or_update"), Some("attributes.hostname"))
    );
    assert_eq!(
        m["columns"][2]["target"],
        json!({ "kind": "relationship", "typeKey": "runs_on", "direction": "outgoing", "match": { "by": "label" } })
    );
    assert_eq!(m["columns"][3]["target"]["direction"], "incoming");
    assert!(v["savedMapping"].is_null());
    // The suggestion is a mapping the job accepts.
    let (status, j, _) = call(&e.app, "PUT", &format!("/api/v1/imports/{id}/mapping"), &alice, Some(m.clone())).await;
    assert_eq!(status, 200, "{j}");

    // An administrator gets the ident column, and it becomes the key.
    let (status, v, _) = call(&e.app, "GET", &suggest, &e.admin, None).await;
    assert_eq!((status, &v["matchedBy"][5]["via"]), (200, &key), "{v}");
    assert_eq!(v["mapping"]["key"]["field"], "ident");

    // A saved mapping with exactly the file's headers is picked by itself.
    // "Cores" and "cores" are one header to a definition.
    let distinct: Vec<&str> = headers.iter().copied().filter(|h| *h != "cores").collect();
    let notes = json!([{ "header": "Notes", "target": { "kind": "attribute", "key": "cores" } }]);
    let body = json!({ "name": "Vendor", "classKey": "srv", "definition": definition(&distinct, notes) });
    let (status, first, _) = call(&e.app, "POST", "/api/v1/import-mappings", &alice, Some(body)).await;
    assert_eq!(status, 201, "{first}");
    let (_, v, _) = call(&e.app, "GET", &suggest, &alice, None).await;
    assert_eq!(v["savedMapping"], json!({ "id": first["id"], "name": "Vendor", "byHeaders": true }), "{v}");
    assert!(vias(&v).iter().all(|(via, _)| *via == saved), "{v}");
    let cols = v["mapping"]["columns"].as_array().unwrap();
    let notes_col = cols.iter().find(|c| c["index"] == 8).unwrap();
    assert_eq!(notes_col["target"], json!({ "kind": "attribute", "key": "cores" }));

    // Two with the same headers: none is picked unless asked for by id.
    let body = json!({ "name": "Other", "classKey": "srv", "definition": definition(&distinct, json!([])) });
    let (status, second, _) = call(&e.app, "POST", "/api/v1/import-mappings", &alice, Some(body)).await;
    assert_eq!(status, 201, "{second}");
    let (_, v, _) = call(&e.app, "GET", &suggest, &alice, None).await;
    assert!(v["savedMapping"].is_null(), "{v}");
    let second_id = second["id"].as_str().unwrap();
    let (_, v, _) = call(&e.app, "GET", &format!("{suggest}&mappingId={second_id}"), &alice, None).await;
    assert_eq!(
        (v["savedMapping"]["name"].as_str(), v["savedMapping"]["byHeaders"].as_bool()),
        (Some("Other"), Some(false))
    );

    // A mapping of another class, and a class alice cannot import into, are refused.
    let body = json!({ "name": "VM", "classKey": "vm", "definition": definition(&["hostname"], json!([])) });
    let (_, vm_map, _) = call(&e.app, "POST", "/api/v1/import-mappings", &alice, Some(body)).await;
    let (status, v, _) =
        call(&e.app, "GET", &format!("{suggest}&mappingId={}", vm_map["id"].as_str().unwrap()), &alice, None).await;
    assert_eq!((status, detail(&v)), (400, "mapping_class_mismatch"), "{v}");
    let (status, v, _) =
        call(&e.app, "GET", &format!("/api/v1/imports/{id}/mapping-suggestion?classKey=nope"), &alice, None).await;
    assert_eq!((status, detail(&v)), (400, "unknown_class"), "{v}");
}
