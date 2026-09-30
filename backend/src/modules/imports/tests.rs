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
    sqlx::query(
        "INSERT INTO permission_profile_class_permissions (profile_id, class_id, can_view, can_create, can_edit, can_delete)
         VALUES ($1, NULL, true, true, true, true)",
    )
    .bind(profile)
    .execute(&e.pool)
    .await
    .unwrap();
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
        tokio::time::sleep(Duration::from_millis(40)).await;
        // Between two bytes the upload holds no pool connection.
        assert_eq!(e.pool.size() as usize - e.pool.num_idle(), 0, "a connection is held while waiting for the client");
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
