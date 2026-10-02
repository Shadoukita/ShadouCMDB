//! Layout templates, class defaults and CIs' own layouts through the real router (SHAA-1472).

use axum::Router;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

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
            { "classKey": "business_service", "className": "Business service", "templateKey": "standard", "explicit": false },
            { "classKey": "server", "className": "Server", "templateKey": "server", "explicit": true }
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
