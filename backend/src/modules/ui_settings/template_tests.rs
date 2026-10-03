//! Layout templates, class defaults and CIs' own layouts through the real router (SHAA-1472).

use axum::Router;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::document;
use crate::db::scratch;
use crate::modules::api_tokens::tests::{Creds, app, call, code, session_of};

struct World {
    app: Router,
    admin: Creds,
    pool: PgPool,
    server: Uuid,
}

async fn world(db: &scratch::Scratch) -> World {
    let app = app(db.pool.clone());
    let setup = json!({ "username": "owner", "displayName": "Owner", "password": "correct horse battery", "setupToken": crate::auth::setup_token::TEST_TOKEN });
    let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
    assert_eq!(status, 201, "{me}");
    let admin = session_of(&me, &headers);
    let (status, v, _) =
        call(&app, "POST", "/api/v1/ci-classes", &admin, Some(json!({ "key": "server", "name": "Server" }))).await;
    assert_eq!(status, 201, "{v}");
    let server: Uuid = v["id"].as_str().unwrap().parse().unwrap();
    let field = json!({ "classId": server, "key": "cpu_cores", "label": "CPU cores", "dataType": "number" });
    let (status, v, _) = call(&app, "POST", "/api/v1/attribute-definitions", &admin, Some(field)).await;
    assert_eq!(status, 201, "{v}");
    World { app, admin, pool: db.pool.clone(), server }
}

impl World {
    async fn ci(&self) -> Uuid {
        let (status, v, _) = call(
            &self.app,
            "POST",
            "/api/v1/configuration-items",
            &self.admin,
            Some(json!({ "classId": self.server })),
        )
        .await;
        assert_eq!(status, 201, "{v}");
        v["id"].as_str().unwrap().parse().unwrap()
    }

    async fn settings(&self) -> Value {
        let (status, v, _) = call(&self.app, "GET", "/api/v1/ui-settings", &self.admin, None).await;
        assert_eq!(status, 200, "{v}");
        v
    }

    /// The current version's document, as the layout editor loads it.
    async fn stored(&self) -> Value {
        let version = self.settings().await["version"].as_i64().unwrap();
        let path = format!("/api/v1/ui-settings/versions/{version}");
        let (status, v, _) = call(&self.app, "GET", &path, &self.admin, None).await;
        assert_eq!(status, 200, "{v}");
        v["settings"].clone()
    }

    async fn put(&self, settings: Value) -> (u16, Value) {
        let version = self.settings().await["version"].as_i64().unwrap();
        let (status, v, _) = call(
            &self.app,
            "PUT",
            "/api/v1/ui-settings",
            &self.admin,
            Some(json!({ "version": version, "settings": settings })),
        )
        .await;
        (status, v)
    }

    async fn raw(&self) -> Value {
        sqlx::query_scalar("SELECT settings FROM ui_settings").fetch_one(&self.pool).await.unwrap()
    }

    /// A signed-in user whose profile has these rights on the server class (view, edit) and these global ones.
    async fn user(&self, name: &str, class: Option<bool>, global: &[&str]) -> Creds {
        let profile: Uuid = sqlx::query_scalar("INSERT INTO permission_profiles (name) VALUES ($1) RETURNING id")
            .bind(format!("profile {name}"))
            .fetch_one(&self.pool)
            .await
            .unwrap();
        if let Some(edit) = class {
            sqlx::query(
                "INSERT INTO permission_profile_class_permissions (profile_id, class_id, can_view, can_edit)
                 VALUES ($1, $2, true, $3)",
            )
            .bind(profile)
            .bind(self.server)
            .bind(edit)
            .execute(&self.pool)
            .await
            .unwrap();
        }
        for g in global {
            sqlx::query("INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES ($1, $2)")
                .bind(profile)
                .bind(g)
                .execute(&self.pool)
                .await
                .unwrap();
        }
        let body = json!({ "username": name, "displayName": format!("User {name}"), "password": "a long enough password", "profileIds": [profile] });
        let (status, v, _) = call(&self.app, "POST", "/api/v1/admin/users", &self.admin, Some(body)).await;
        assert_eq!(status, 201, "{v}");
        let login = json!({ "username": name, "password": "a long enough password" });
        let (status, me, headers) = call(&self.app, "POST", "/api/v1/auth/login", &Creds::default(), Some(login)).await;
        assert_eq!(status, 200, "{me}");
        session_of(&me, &headers)
    }
}

fn tab(label: &str, fields: &[&str]) -> Value {
    let fields: Vec<Value> = fields.iter().map(|f| json!({ "field": f })).collect();
    json!({ "key": "main", "label": label, "sections": [{ "key": "core", "label": "Core", "fields": fields }] })
}

fn keys(templates: &Value) -> Vec<&str> {
    templates.as_array().unwrap().iter().map(|t| t["key"].as_str().unwrap()).collect()
}

fn template<'a>(settings: &'a Value, key: &str) -> &'a Value {
    settings["layoutTemplates"].as_array().unwrap().iter().find(|t| t["key"] == key).unwrap_or(&Value::Null)
}

/// A class layout sent the way the layout editor (and an older API client) sends it becomes a template; the
/// settings the API returns carry it on the class again, so sending them back changes nothing.
#[tokio::test]
async fn class_layouts_are_stored_as_templates_and_read_back_on_the_class() {
    let Some(db) = scratch::database("class_layouts_are_stored_as_templates_and_read_back_on_the_class").await else {
        return;
    };
    let w = world(&db).await;
    let sent = json!({ "classKey": "server", "tabs": [tab("Main", &["ident", "attributes.cpu_cores"])],
                       "readOnlyFields": ["label"] });
    let (status, v) = w.put(json!({ "layouts": [sent] })).await;
    assert_eq!(status, 200, "{v}");

    // Stored: the class names its template, which holds the layout.
    let raw = w.raw().await;
    assert_eq!(raw["layouts"], json!([{ "classKey": "server", "templateKey": "server" }]), "{raw}");
    assert_eq!(keys(&raw["layoutTemplates"]), ["standard", "server"]);
    assert_eq!(template(&raw, "standard")["name"], "Standard");
    assert_eq!(template(&raw, "server")["name"], "Server layout");
    let layout = &template(&raw, "server")["layout"];
    let fields: Vec<&str> = layout["tabs"][0]["sections"][0]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["field"].as_str().unwrap())
        .collect();
    assert_eq!(fields, ["ident", "attributes.cpu_cores"], "no field lost");
    assert_eq!(layout["readOnlyFields"], json!(["label"]));

    // Returned: the class carries the template's layout, in the effective settings and the stored version.
    let effective = w.settings().await;
    let stored = w.stored().await;
    for doc in [&effective["settings"], &stored] {
        assert_eq!(doc["layouts"][0]["templateKey"], "server");
        assert_eq!(doc["layouts"][0]["tabs"], layout["tabs"], "{doc}");
        assert_eq!(doc["layouts"][0]["readOnlyFields"], json!(["label"]));
    }

    // Sent back unchanged: no new version.
    let version = effective["version"].as_i64().unwrap();
    let (status, v) = w.put(stored.clone()).await;
    assert_eq!((status, v["version"].as_i64()), (200, Some(version)), "{v}");

    // Edited on the class (the layout editor): the template changes.
    let mut edit = stored.clone();
    edit["layouts"][0]["tabs"][0]["label"] = json!("Overview");
    let (status, v) = w.put(edit).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(template(&w.raw().await, "server")["layout"]["tabs"][0]["label"], "Overview");

    // Edited in the template while the class still carries the old layout: the template edit counts.
    let stored = w.stored().await;
    let mut edit = stored.clone();
    let i = keys(&edit["layoutTemplates"]).iter().position(|k| *k == "server").unwrap();
    edit["layoutTemplates"][i]["layout"]["tabs"][0]["label"] = json!("Summary");
    edit["layoutTemplates"][i]["name"] = json!("Server detail page");
    let (status, v) = w.put(edit.clone()).await;
    assert_eq!(status, 200, "{v}");
    let raw = w.raw().await;
    assert_eq!(template(&raw, "server")["layout"]["tabs"][0]["label"], "Summary");
    assert_eq!(template(&raw, "server")["name"], "Server detail page", "renamed, same key");

    // Both edited differently in one request: refused.
    let mut both = w.stored().await;
    both["layoutTemplates"][i]["layout"]["tabs"][0]["label"] = json!("A");
    both["layouts"][0]["tabs"][0]["label"] = json!("B");
    let (status, v) = w.put(both).await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");
    assert_eq!(v["error"]["details"][0]["field"], "settings.layouts.0.tabs", "{v}");

    // A template that does not exist, and names that differ only in case.
    let mut unknown = w.stored().await;
    unknown["layouts"][0] = json!({ "classKey": "server", "templateKey": "nope" });
    let (status, v) = w.put(unknown).await;
    assert_eq!(status, 400, "{v}");
    assert_eq!(v["error"]["details"][0]["field"], "settings.layouts.0.templateKey", "{v}");
    let mut twins = w.stored().await;
    twins["layoutTemplates"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "key": "other", "name": "SERVER DETAIL PAGE", "layout": {} }));
    let (status, v) = w.put(twins).await;
    assert_eq!(status, 400, "{v}");
    assert_eq!(v["error"]["details"][0]["field"], "settings.layoutTemplates.2.name", "{v}");

    db.drop().await;
}

#[tokio::test]
async fn cis_use_another_template_or_their_own_layout_and_used_templates_stay() {
    let Some(db) = scratch::database("cis_use_another_template_or_their_own_layout_and_used_templates_stay").await
    else {
        return;
    };
    let w = world(&db).await;
    let compact = json!({ "key": "compact", "name": "Compact", "description": "Two fields only",
                          "layout": { "tabs": [tab("Compact", &["ident", "label"])] } });
    let (status, v) = w
        .put(json!({ "layouts": [{ "classKey": "server", "tabs": [tab("Main", &["ident"])] }],
                     "layoutTemplates": [compact] }))
        .await;
    assert_eq!(status, 200, "{v}");
    let ci = w.ci().await;
    let path = format!("/api/v1/configuration-items/{ci}/layout");

    // The class default.
    let (status, v, _) = call(&w.app, "GET", &path, &w.admin, None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!((v["source"].as_str(), v["templateKey"].as_str()), (Some("class_default"), Some("server")), "{v}");
    assert_eq!(v["classTemplateKey"], "server");
    assert_eq!(v["layout"]["tabs"][0]["label"], "Main");
    assert_eq!(v["version"], Value::Null);

    // Another template.
    let (status, v, _) = call(&w.app, "PUT", &path, &w.admin, Some(json!({ "templateKey": "compact" }))).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!((v["source"].as_str(), v["templateName"].as_str()), (Some("template"), Some("Compact")), "{v}");
    assert_eq!((v["layout"]["tabs"][0]["label"].as_str(), v["version"].as_i64()), (Some("Compact"), Some(1)));
    let (status, v, _) = call(&w.app, "PUT", &path, &w.admin, Some(json!({ "templateKey": "nope" }))).await;
    assert_eq!((status, v["error"]["details"][0]["field"].as_str()), (400, Some("templateKey")), "{v}");
    let both = json!({ "templateKey": "compact", "layout": {} });
    let (status, v, _) = call(&w.app, "PUT", &path, &w.admin, Some(both)).await;
    assert_eq!(status, 400, "{v}");

    // Usage: the class and the CI.
    let (status, u, _) = call(&w.app, "GET", "/api/v1/ui-settings/layout-templates/usage", &w.admin, None).await;
    assert_eq!(status, 200, "{u}");
    let usage = |u: &Value, key: &str| {
        let t = u["templates"].as_array().unwrap().iter().find(|t| t["key"] == key).unwrap().clone();
        (t["classKeys"].clone(), t["overrideCount"].clone())
    };
    assert_eq!(usage(&u, "compact"), (json!([]), json!(1)), "{u}");
    assert_eq!(usage(&u, "server"), (json!(["server"]), json!(0)), "{u}");
    // The built-in business service class has no layout entry: Standard.
    assert_eq!(usage(&u, "standard"), (json!(["business_service"]), json!(0)), "{u}");
    assert_eq!(
        u["classes"],
        json!([
            { "classKey": "business_service", "className": "Business service", "templateKey": "standard", "explicit": false, "ownLayoutCount": 0 },
            { "classKey": "server", "className": "Server", "templateKey": "server", "explicit": true, "ownLayoutCount": 1 }
        ])
    );

    // Templates in use cannot be removed: 409 with who uses them.
    let mut gone = w.stored().await;
    gone["layoutTemplates"] = json!([]);
    gone["layouts"][0] = json!({ "classKey": "server", "templateKey": "server" });
    let (status, v) = w.put(gone).await;
    assert_eq!((status, code(&v)), (409, "CONFLICT"), "{v}");
    let messages: Vec<&str> =
        v["error"]["details"].as_array().unwrap().iter().map(|d| d["message"].as_str().unwrap()).collect();
    assert_eq!(
        messages,
        [
            "Template \"Compact\" (compact) is used by 1 configuration item",
            "Template \"Server layout\" (server) is used by class server"
        ],
        "{v}"
    );
    assert_eq!(v["error"]["details"][0]["code"], "in_use");

    // A layout of its own: unknown attributes are left out and reported.
    let own =
        json!({ "version": 1, "layout": { "tabs": [tab("Mine", &["attributes.cpu_cores", "attributes.gone"])] } });
    let (status, v, _) = call(&w.app, "PUT", &path, &w.admin, Some(own)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        (v["source"].as_str(), v["templateKey"].as_str(), v["version"].as_i64()),
        (Some("custom"), None, Some(2))
    );
    assert_eq!(
        v["layout"]["tabs"][0]["sections"][0]["fields"],
        json!([{ "field": "attributes.cpu_cores", "width": 1 }])
    );
    assert_eq!(v["issues"][0]["path"], "layout.tabs.0.sections.0.fields.1", "{v}");
    assert_eq!(v["issues"][0]["code"], "unknown_attribute");
    // Stale version.
    let (status, v, _) =
        call(&w.app, "PUT", &path, &w.admin, Some(json!({ "version": 1, "templateKey": "compact" }))).await;
    assert_eq!((status, code(&v)), (409, "VERSION_CONFLICT"), "{v}");

    // Compact is free now; the class default moves to Standard, so the server template is free too.
    let (status, v, _) = call(
        &w.app,
        "PUT",
        "/api/v1/ui-settings/class-layouts/server",
        &w.admin,
        Some(json!({ "templateKey": "standard" })),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["settings"]["layouts"][0]["templateKey"], "standard", "{v}");
    let (status, v, _) = call(
        &w.app,
        "PUT",
        "/api/v1/ui-settings/class-layouts/nope",
        &w.admin,
        Some(json!({ "templateKey": "standard" })),
    )
    .await;
    assert_eq!(status, 404, "{v}");
    let mut gone = w.stored().await;
    gone["layoutTemplates"] = json!([template(&gone, "standard").clone()]);
    let (status, v) = w.put(gone).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(keys(&w.raw().await["layoutTemplates"]), ["standard"]);

    // Reset: back to the class default.
    let (status, _, _) = call(&w.app, "DELETE", &path, &w.admin, None).await;
    assert_eq!(status, 204);
    let (_, v, _) = call(&w.app, "GET", &path, &w.admin, None).await;
    assert_eq!((v["source"].as_str(), v["templateKey"].as_str()), (Some("class_default"), Some("standard")), "{v}");
    let (status, _, _) = call(&w.app, "DELETE", &path, &w.admin, None).await;
    assert_eq!(status, 204, "resetting twice is fine");

    // Every write is audited against the CI.
    let (status, log, _) =
        call(&w.app, "GET", "/api/v1/audit-log?entityType=ci_layout_overrides&limit=10", &w.admin, None).await;
    assert_eq!(status, 200, "{log}");
    let actions: Vec<(&str, &str)> = log["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["action"].as_str().unwrap(), e["entityId"].as_str().unwrap()))
        .collect();
    let id = ci.to_string();
    assert_eq!(actions, [("delete", id.as_str()), ("update", id.as_str()), ("create", id.as_str())], "{log}");
    assert_eq!(log["data"][2]["newValue"]["templateKey"], "compact");
    assert_eq!(log["data"][0]["oldValue"]["layout"]["tabs"][0]["label"], "Mine");

    // A deleted CI keeps its layout but does not hold a template.
    let (status, v, _) = call(&w.app, "PUT", &path, &w.admin, Some(json!({ "templateKey": "standard" }))).await;
    assert_eq!(status, 200, "{v}");
    let (status, _, _) = call(&w.app, "DELETE", &format!("/api/v1/configuration-items/{ci}"), &w.admin, None).await;
    assert_eq!(status, 204);
    let (status, v, _) = call(&w.app, "GET", &path, &w.admin, None).await;
    assert_eq!((status, v["source"].as_str()), (200, Some("template")), "{v}");
    let (status, _, _) = call(&w.app, "PUT", &path, &w.admin, Some(json!({ "templateKey": "standard" }))).await;
    assert_eq!(status, 404, "deleted CIs are not edited");
    let (_, u, _) = call(&w.app, "GET", "/api/v1/ui-settings/layout-templates/usage", &w.admin, None).await;
    assert_eq!(usage(&u, "standard"), (json!(["business_service", "server"]), json!(0)), "{u}");

    db.drop().await;
}

#[tokio::test]
async fn layout_changes_need_customization_and_edit_on_the_class() {
    let Some(db) = scratch::database("layout_changes_need_customization_and_edit_on_the_class").await else { return };
    let w = world(&db).await;
    let ci = w.ci().await;
    let path = format!("/api/v1/configuration-items/{ci}/layout");
    let set = json!({ "templateKey": "standard" });

    let editor = w.user("editor", Some(true), &[]).await;
    let viewer_admin = w.user("viewer_admin", Some(false), &["customization.manage"]).await;
    let blind_admin = w.user("blind_admin", None, &["customization.manage", "audit.view"]).await;
    let designer = w.user("designer", Some(true), &["customization.manage"]).await;

    // Reading needs view on the class, like the CI.
    let (status, _, _) = call(&w.app, "GET", &path, &editor, None).await;
    assert_eq!(status, 200);
    let (status, _, _) = call(&w.app, "GET", &path, &blind_admin, None).await;
    assert_eq!(status, 404);

    for (who, creds, expected) in
        [("editor", &editor, 403), ("viewer_admin", &viewer_admin, 403), ("blind_admin", &blind_admin, 404)]
    {
        let (status, v, _) = call(&w.app, "PUT", &path, creds, Some(set.clone())).await;
        assert_eq!(status, expected, "{who} PUT: {v}");
        let (status, v, _) = call(&w.app, "DELETE", &path, creds, None).await;
        assert_eq!(status, expected, "{who} DELETE: {v}");
    }
    let (status, v, _) = call(&w.app, "GET", "/api/v1/ui-settings/layout-templates/usage", &editor, None).await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");
    let (status, _, _) =
        call(&w.app, "PUT", "/api/v1/ui-settings/class-layouts/server", &editor, Some(set.clone())).await;
    assert_eq!(status, 403);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM ci_layout_overrides").fetch_one(&w.pool).await.unwrap(),
        0
    );

    let (status, v, _) = call(&w.app, "PUT", &path, &designer, Some(set.clone())).await;
    assert_eq!(status, 200, "{v}");

    // The audit entry stays with the CI: a reader who may not view its class does not see it.
    let (status, log, _) =
        call(&w.app, "GET", "/api/v1/audit-log?entityType=ci_layout_overrides", &blind_admin, None).await;
    assert_eq!(status, 200, "{log}");
    assert_eq!(log["data"], json!([]), "{log}");
    let (_, log, _) = call(&w.app, "GET", "/api/v1/audit-log?entityType=ci_layout_overrides", &w.admin, None).await;
    assert_eq!(log["data"][0]["actorName"], "designer", "{log}");

    db.drop().await;
}

/// An export made before templates (class layouts holding their tabs, no `layoutTemplates`) imports as the
/// migration converts stored settings; a template a CI still uses is kept.
#[tokio::test]
async fn an_export_from_before_templates_imports_as_templates() {
    let Some(db) = scratch::database("an_export_from_before_templates_imports_as_templates").await else { return };
    let w = world(&db).await;
    let used = json!({ "key": "used", "name": "Used by a CI", "layout": {} });
    let (status, v) = w.put(json!({ "layoutTemplates": [used] })).await;
    assert_eq!(status, 200, "{v}");
    let ci = w.ci().await;
    let path = format!("/api/v1/configuration-items/{ci}/layout");
    let (status, v, _) = call(&w.app, "PUT", &path, &w.admin, Some(json!({ "templateKey": "used" }))).await;
    assert_eq!(status, 200, "{v}");

    let (status, mut file, _) = call(&w.app, "GET", "/api/v1/admin/config/export", &w.admin, None).await;
    assert_eq!(status, 200, "{file}");
    let old_tabs = json!([tab("Main", &["attributes.cpu_cores", "ident"])]);
    file["uiSettings"]["settings"] = json!({ "branding": { "appName": "Acme CMDB" },
        "layouts": [{ "classKey": "server", "tabs": old_tabs.clone(), "hiddenFields": ["label"] }] });
    let (status, v, _) = call(&w.app, "POST", "/api/v1/admin/config/import?mode=apply", &w.admin, Some(file)).await;
    assert_eq!(status, 200, "{v}");

    let raw = w.raw().await;
    assert_eq!(raw["layouts"], json!([{ "classKey": "server", "templateKey": "server" }]), "{raw}");
    assert_eq!(keys(&raw["layoutTemplates"]), ["standard", "used", "server"], "{raw}");
    let server = &template(&raw, "server");
    assert_eq!(server["name"], "Server layout");
    let fields = &server["layout"]["tabs"][0]["sections"][0]["fields"];
    assert_eq!(fields, &json!([{ "field": "attributes.cpu_cores", "width": 1 }, { "field": "ident", "width": 1 }]));
    assert_eq!(server["layout"]["hiddenFields"], json!(["label"]));
    assert_eq!(raw["branding"]["appName"], "Acme CMDB");
    let (_, v, _) = call(&w.app, "GET", &path, &w.admin, None).await;
    assert_eq!((v["source"].as_str(), v["templateKey"].as_str()), (Some("template"), Some("used")), "{v}");

    db.drop().await;
}

/// Templates and CIs' own layouts are free like class layouts (SHAA-1471): a tab sent as `grid` is stored
/// free, and a CI layout stored on the grid before free placement was the only one comes back free.
#[tokio::test]
async fn template_and_ci_layouts_on_the_grid_come_back_free() {
    let Some(db) = scratch::database("template_and_ci_layouts_on_the_grid_come_back_free").await else { return };
    let w = world(&db).await;
    let mut grid = tab("Compact", &["ident", "label"]);
    grid["placement"] = json!("grid");
    let compact = json!({ "key": "compact", "name": "Compact", "layout": { "tabs": [grid.clone()] } });
    let (status, v) = w.put(json!({ "layoutTemplates": [compact] })).await;
    assert_eq!(status, 200, "{v}");
    let stored = template(&w.raw().await, "compact")["layout"]["tabs"][0].clone();
    assert_eq!(stored["placement"], "free", "{stored}");
    assert_eq!(stored["sections"][0]["frame"]["w"], 1.0, "{stored}");
    assert_eq!(stored["sections"][0]["fields"].as_array().unwrap().len(), 2);

    let ci = w.ci().await;
    let path = format!("/api/v1/configuration-items/{ci}/layout");
    let (status, v, _) =
        call(&w.app, "PUT", &path, &w.admin, Some(json!({ "layout": { "tabs": [grid.clone()] } }))).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["layout"]["tabs"][0]["placement"], "free", "{v}");
    // As a row written before this change holds it.
    sqlx::query("UPDATE ci_layout_overrides SET layout = $1")
        .bind(json!({ "tabs": [grid] }))
        .execute(&w.pool)
        .await
        .unwrap();
    let (status, v, _) = call(&w.app, "GET", &path, &w.admin, None).await;
    assert_eq!(status, 200, "{v}");
    let tab = &v["layout"]["tabs"][0];
    assert_eq!(
        (tab["placement"].as_str(), tab["sections"][0]["frame"]["x"].as_f64()),
        (Some("free"), Some(0.0)),
        "{v}"
    );
    assert_eq!(tab["sections"][0]["fields"].as_array().unwrap().len(), 2, "{v}");

    db.drop().await;
}

/// What the admin tables link to: per class the CIs with their own layout, per template a CI that shows it,
/// and the inventory list filtered by either (SHAA-1504).
#[tokio::test]
async fn usage_counts_own_layouts_names_a_sample_ci_and_the_list_filters_by_them() {
    let Some(db) = scratch::database("usage_counts_own_layouts_names_a_sample_ci_and_the_list_filters_by_them").await
    else {
        return;
    };
    let w = &world(&db).await;
    let compact = json!({ "key": "compact", "name": "Compact", "layout": { "tabs": [tab("Compact", &["ident"])] } });
    let (status, v) = w.put(json!({ "layouts": [], "layoutTemplates": [compact] })).await;
    assert_eq!(status, 200, "{v}");
    let (a, b, c, plain, deleted) = (w.ci().await, w.ci().await, w.ci().await, w.ci().await, w.ci().await);
    let set = |id: Uuid, body: Value| {
        let path = format!("/api/v1/configuration-items/{id}/layout");
        let app = w.app.clone();
        let admin = w.admin.clone();
        async move {
            let (status, v, _) = call(&app, "PUT", &path, &admin, Some(body)).await;
            assert_eq!(status, 200, "{v}");
        }
    };
    set(a, json!({ "templateKey": "compact" })).await;
    set(b, json!({ "templateKey": "compact" })).await;
    set(c, json!({ "layout": { "tabs": [tab("Mine", &["label"])] } })).await;
    set(deleted, json!({ "templateKey": "compact" })).await;
    let (status, v, _) =
        call(&w.app, "DELETE", &format!("/api/v1/configuration-items/{deleted}"), &w.admin, None).await;
    assert!(status == 204 || status == 200, "{v}");
    let label = |id: Uuid| async move {
        let (_, v, _) = call(&w.app, "GET", &format!("/api/v1/configuration-items/{id}"), &w.admin, None).await;
        v["label"].as_str().unwrap().to_owned()
    };
    let first = if label(a).await.to_lowercase() <= label(b).await.to_lowercase() { a } else { b };

    let usage = |creds: Creds| async move {
        let (status, u, _) = call(&w.app, "GET", "/api/v1/ui-settings/layout-templates/usage", &creds, None).await;
        assert_eq!(status, 200, "{u}");
        let t = |key: &str| u["templates"].as_array().unwrap().iter().find(|t| t["key"] == key).unwrap().clone();
        let class = |key: &str| u["classes"].as_array().unwrap().iter().find(|c| c["classKey"] == key).unwrap().clone();
        (t("compact"), t("standard"), class("server"))
    };
    let (compact, standard, server) = usage(w.admin.clone()).await;
    // Live CIs only: the deleted one counts nowhere and is never the sample.
    assert_eq!(
        (compact["overrideCount"].as_i64(), compact["sampleCiId"].as_str()),
        (Some(2), Some(&*first.to_string()))
    );
    assert_eq!((standard["overrideCount"].as_i64(), &standard["sampleCiId"]), (Some(0), &Value::Null));
    assert_eq!(server["ownLayoutCount"], 3, "{server}");

    // A manager who may not view servers learns neither the counts nor a CI.
    let blind = w.user("blind", None, &["customization.manage"]).await;
    let (compact, _, server) = usage(blind).await;
    assert_eq!((&compact["overrideCount"], &compact["sampleCiId"]), (&Value::Null, &Value::Null), "{compact}");
    assert_eq!(server["ownLayoutCount"], Value::Null, "{server}");

    // The inventory list behind the links.
    let ids = |query: &'static str| async move {
        let path = format!("/api/v1/configuration-items?classId={}&includeSubclasses=false&{query}", w.server);
        let (status, v, _) = call(&w.app, "GET", &path, &w.admin, None).await;
        (status, v)
    };
    let sorted = |v: &Value| {
        let mut out: Vec<Uuid> =
            v["data"].as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap().parse().unwrap()).collect();
        out.sort();
        (out, v["page"]["total"].as_i64().unwrap())
    };
    let set_of = |mut ids: Vec<Uuid>| {
        ids.sort();
        let n = ids.len() as i64;
        (ids, n)
    };
    let (status, v) = ids("ownLayout=true").await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(sorted(&v), set_of(vec![a, b, c]));
    let (_, v) = ids("ownLayout=false").await;
    assert_eq!(sorted(&v), set_of(vec![plain]));
    let (_, v) = ids("layoutTemplate=compact").await;
    assert_eq!(sorted(&v), set_of(vec![a, b]));
    let (_, v) = ids("layoutTemplate=compact&deleted=only").await;
    assert_eq!(sorted(&v), set_of(vec![deleted]));
    let (_, v) = ids("layoutTemplate=standard").await;
    assert_eq!(sorted(&v), set_of(vec![]));
    let (status, v) = ids("layoutTemplate=Not%20a%20key").await;
    assert_eq!((status, v["error"]["details"][0]["field"].as_str()), (400, Some("layoutTemplate")), "{v}");
    let (status, v) = ids("ownLayout=maybe").await;
    assert_eq!(status, 400, "{v}");

    db.drop().await;
}

/// A PUT that leaves the Standard template out keeps it as it was, with its layout, name and description,
/// instead of resetting it to an empty layout for every class and CI using it (GH#521).
#[tokio::test]
async fn a_put_without_the_standard_template_keeps_its_layout() {
    let Some(db) = scratch::database("a_put_without_the_standard_template_keeps_its_layout").await else {
        return;
    };
    let w = world(&db).await;
    let mut settings = w.stored().await;
    let i = keys(&settings["layoutTemplates"]).iter().position(|k| *k == "standard").unwrap();
    settings["layoutTemplates"][i]["layout"]["hiddenFields"] = json!(["attributes.cpu_cores"]);
    settings["layoutTemplates"][i]["description"] = json!("Every class without its own template");
    let (status, v) = w.put(settings).await;
    assert_eq!(status, 200, "{v}");
    let before = template(&w.raw().await, "standard").clone();
    assert_eq!(before["layout"]["hiddenFields"], json!(["attributes.cpu_cores"]), "{before}");

    // Only another template sent: Standard is carried over.
    let compact = json!({ "key": "compact", "name": "Compact", "layout": { "tabs": [tab("Compact", &["ident"])] } });
    let (status, v) = w.put(json!({ "layoutTemplates": [compact] })).await;
    assert_eq!(status, 200, "{v}");
    let raw = w.raw().await;
    assert_eq!(keys(&raw["layoutTemplates"]), ["standard", "compact"]);
    assert_eq!(template(&raw, "standard"), &before, "{raw}");
    assert_eq!(
        w.settings().await["settings"]["layoutTemplates"][0]["layout"]["hiddenFields"],
        json!(["attributes.cpu_cores"])
    );

    // A class edits Standard through its layout while the template itself is left out: the edit counts.
    let sent = json!({ "classKey": "server", "templateKey": "standard", "readOnlyFields": ["label"] });
    let (status, v) = w.put(json!({ "layouts": [sent], "layoutTemplates": [template(&raw, "compact")] })).await;
    assert_eq!(status, 200, "{v}");
    let standard = template(&w.raw().await, "standard").clone();
    assert_eq!(standard["layout"]["readOnlyFields"], json!(["label"]), "{standard}");
    assert_eq!(standard["name"], "Standard");

    // Standard sent with an empty layout: that clears it.
    let empty = json!({ "key": "standard", "name": "Standard", "layout": {} });
    let (status, v) = w.put(json!({ "layoutTemplates": [empty] })).await;
    assert_eq!(status, 200, "{v}");
    let raw = w.raw().await;
    assert_eq!(keys(&raw["layoutTemplates"]), ["standard"]);
    let standard = template(&raw, "standard");
    assert!(standard["layout"].get("readOnlyFields").is_none_or(|f| f == &json!([])), "{standard}");
    assert!(standard["layout"].get("hiddenFields").is_none_or(|f| f == &json!([])), "{standard}");

    db.drop().await;
}

/// A layout of `notes` note sections of random text (8 to a tab), the last one `last` characters long:
/// random text compresses badly, so the database's own check would be the first to see it.
fn noisy_layout(notes: usize, last: usize) -> Value {
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let mut text = |len: usize| -> String {
        (0..len)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                char::from(b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"[(seed % 62) as usize])
            })
            .collect()
    };
    let mut tabs: Vec<Value> = Vec::new();
    for i in 0..notes {
        if i % 8 == 0 {
            tabs.push(json!({ "key": format!("t{i}"), "label": "Notes", "sections": [] }));
        }
        let len = if i + 1 == notes { last } else { document::NOTE_MAX_CHARS };
        let section = json!({ "key": format!("n{i}"), "label": "Note", "kind": "note", "text": text(len) });
        tabs.last_mut().unwrap()["sections"].as_array_mut().unwrap().push(section);
    }
    json!({ "tabs": tabs })
}

/// The layout's size as the server counts it: stored (normalised) JSON.
fn stored_size(layout: &Value) -> usize {
    let l: document::UiLayout = serde_json::from_value(layout.clone()).unwrap();
    serde_json::to_vec(&l.normalized()).unwrap().len()
}

/// GH#533: a CI's own layout and a template are limited to 256 KiB before the database's check on
/// `ci_layout_overrides.layout` is reached, with an error on the field instead of the constraint's text.
#[tokio::test]
async fn layouts_over_256_kib_are_refused_on_the_field() {
    let Some(db) = scratch::database("layouts_over_256_kib_are_refused_on_the_field").await else { return };
    let w = world(&db).await;
    let ci = w.ci().await;
    let path = format!("/api/v1/configuration-items/{ci}/layout");

    // The reporter's layout: 20 tabs of 8 random notes (about 650 KB).
    let (status, v, _) = call(&w.app, "PUT", &path, &w.admin, Some(json!({ "layout": noisy_layout(160, 4000) }))).await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");
    let details = &v["error"]["details"];
    assert_eq!(details.as_array().unwrap().len(), 1, "{v}");
    assert_eq!((details[0]["field"].as_str(), details[0]["code"].as_str()), (Some("layout"), Some("too_large")));
    assert_eq!(details[0]["message"], "The layout is larger than 256 KiB");

    // Exactly 256 KiB is stored (the database check does not fire first), one byte more is not.
    let notes =
        (1..).find(|&n| stored_size(&noisy_layout(n, document::NOTE_MAX_CHARS)) >= document::LAYOUT_MAX_BYTES).unwrap();
    let last = document::LAYOUT_MAX_BYTES - stored_size(&noisy_layout(notes, 0));
    let at_limit = noisy_layout(notes, last);
    assert_eq!(stored_size(&at_limit), document::LAYOUT_MAX_BYTES);
    let over = noisy_layout(notes, last + 1);
    let (status, v, _) = call(&w.app, "PUT", &path, &w.admin, Some(json!({ "layout": over.clone() }))).await;
    assert_eq!((status, details_code(&v)), (400, Some("too_large")), "{v}");
    let (_, v, _) = call(&w.app, "GET", &path, &w.admin, None).await;
    assert_eq!(v["source"], "class_default", "nothing stored");
    let (status, v, _) = call(&w.app, "PUT", &path, &w.admin, Some(json!({ "layout": at_limit }))).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["source"], "custom");

    // The same limit for a template of the settings.
    let mut settings = w.stored().await;
    let standard = template(&settings, "standard").clone();
    settings["layoutTemplates"] = json!([standard, { "key": "big", "name": "Big", "layout": over }]);
    let (status, v) = w.put(settings).await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");
    assert_eq!(v["error"]["details"][0]["field"], "settings.layoutTemplates.1.layout", "{v}");
    assert_eq!(v["error"]["details"][0]["code"], "too_large");

    db.drop().await;
}

fn details_code(v: &Value) -> Option<&str> {
    v["error"]["details"][0]["code"].as_str()
}
