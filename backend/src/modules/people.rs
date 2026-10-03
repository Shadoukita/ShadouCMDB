//! Sign-in accounts and their Person CIs (SHAA-1505, migration 0044).
//!
//! Every user with an e-mail is linked to exactly one CI of the built-in
//! Person type (`users.person_ci_id`), and the account's e-mail is the source
//! of truth for the Person's Email:
//!
//! - creating an account, provisioning one through an identity provider, or
//!   giving an account its first e-mail links it to the Person with that
//!   e-mail (a deleted one is restored), or creates one (Name = the display
//!   name). Administrators never make the Person by hand.
//! - changing a linked account's e-mail changes its Person's Email in the same
//!   transaction; when another Person has the new address the change is
//!   refused.
//! - a linked Person's Email cannot be changed on the CI, nor can the Person be
//!   deleted or change its type (`items`); deleting the account leaves the
//!   Person in place, unlinked.
//!
//! The Person writes go through the CI write path (validation, labels, audit
//! rows naming the caller) without asking for class rights: they are part of
//! the account change, which needs users.manage.

use axum::http::Method;
use serde::Serialize;
use serde_json::{Map, Value};
use sqlx::{PgConnection, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::api::context::{Caller, RequestContext};
use crate::api::route::{IdPath, In, Json, NoBody, NoQuery, Route, route};
use crate::auth::permissions::GlobalPermission;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::data::people::{self as data, PersonType};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::modules::items::service as items;
use crate::modules::users;
use crate::schema::model::Model;
use crate::schema::{self as engine, Purge, Scope};

/// The Person type, its table built if a database migrated but not
/// reconciled yet has none (`shadoucmdb migrate` reconciles; a test database
/// or a first start may not have).
pub async fn ready_type(conn: &mut PgConnection, ctx: &RequestContext) -> Result<PersonType, AppError> {
    let model = Model::load(conn).await?;
    let Some(t) = data::person_type(conn, &model).await? else {
        return Err(AppError::new(
            ErrorCode::SchemaNotMigrated,
            "The built-in Person type is missing; run `shadoucmdb migrate`",
        ));
    };
    if !data::table_exists(conn, &t.table).await? {
        let system = RequestContext::system("sign-in accounts", ctx.request_id.clone());
        engine::apply(conn, &system, "Build the Person type", Scope::Classes(vec![t.class_id]), Purge::default())
            .await?;
    }
    Ok(t)
}

/// A refused Person write, told in terms of the account: its e-mail and
/// display name, or the Person fields an account cannot fill.
fn account_error(mut e: AppError, t: &PersonType) -> AppError {
    let email = format!("attributes.{}", t.email.key);
    let name = format!("attributes.{}", t.name.key);
    let Some(details) = e.details.as_mut() else { return e };
    let mut others = Vec::new();
    for d in details.iter_mut() {
        if d.field == email {
            d.field = "email".into();
        } else if d.field == name {
            d.field = "displayName".into();
        } else if let Some(key) = d.field.strip_prefix("attributes.") {
            others.push(key.to_owned());
        }
    }
    if !others.is_empty() {
        return AppError::new(
            ErrorCode::Conflict,
            format!(
                "Every account gets a person automatically, and the Person type requires values an account cannot \
                 provide: {}. Make these fields optional or give them a default value (Data model > Person).",
                others.join(", ")
            ),
        );
    }
    e
}

fn email_taken_by_person(email: &str) -> AppError {
    let message = format!("Another person already has the e-mail address {email}; change that person's e-mail first");
    AppError::new(ErrorCode::Conflict, message.clone()).with_details(vec![FieldError {
        location: FieldLocation::Body,
        field: "email".into(),
        message,
        code: "person_email_taken".into(),
    }])
}

fn email_value(t: &PersonType, email: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(t.email.key.clone(), Value::String(email.to_owned()));
    m
}

/// Links the account to its Person after its e-mail was set or changed in
/// this transaction (see the module documentation). An account without an
/// e-mail is left alone. Writes the audit rows of the Person changes; the
/// caller writes the account's own row, which shows the link.
pub async fn link_user(conn: &mut PgConnection, ctx: &RequestContext, user_id: Uuid) -> Result<(), AppError> {
    let t = ready_type(conn, ctx).await?;
    let a = data::lock_account(conn, user_id).await?.ok_or_else(|| AppError::missing("User", user_id))?;
    let Some(email) = a.email.clone() else { return Ok(()) };
    match a.person_ci_id {
        Some(person) => {
            if data::email_of(conn, &t, person).await?.as_deref() == Some(email.as_str()) {
                return Ok(());
            }
            if data::find_by_email(conn, &t, &email).await?.is_some_and(|other| other.id != person) {
                return Err(email_taken_by_person(&email));
            }
            items::update_on_behalf(conn, ctx, person, email_value(&t, &email))
                .await
                .map_err(|e| account_error(e, &t))?;
        }
        None => {
            let person = match data::find_by_email(conn, &t, &email).await? {
                Some(found) if found.linked_to.is_some() => {
                    // users_email_uq makes this unreachable: the linked account has the same address.
                    return Err(email_taken_by_person(&email));
                }
                Some(found) => {
                    // A Person without an account (decision 6) is adopted.
                    items::restore_on_behalf(conn, ctx, found.id).await?;
                    // The stored address may differ in case; it takes the account's.
                    items::update_on_behalf(conn, ctx, found.id, email_value(&t, &email))
                        .await
                        .map_err(|e| account_error(e, &t))?;
                    found.id
                }
                None => {
                    let mut values = email_value(&t, &email);
                    values.insert(t.name.key.clone(), Value::String(a.display_name.clone()));
                    items::create_on_behalf(conn, ctx, t.class_id, values).await.map_err(|e| account_error(e, &t))?
                }
            };
            data::set_link(conn, user_id, Some(person)).await?;
            tracing::info!(user = %a.username, person = %person, "account linked to its person");
        }
    }
    Ok(())
}

/// Whether `email` can become the address of the account `user_id` (None:
/// a new account): no other account has it ignoring case, and the Person that
/// has it, if any, is the account's own or, for an account without a Person
/// yet, one without an account (which it would adopt).
pub async fn email_usable(conn: &mut PgConnection, user_id: Option<Uuid>, email: &str) -> Result<bool, AppError> {
    if data::email_taken(conn, email, user_id).await? {
        return Ok(false);
    }
    let model = Model::load(conn).await?;
    let Some(t) = data::person_type(conn, &model).await? else { return Ok(true) };
    if !data::table_exists(conn, &t.table).await? {
        return Ok(true);
    }
    let Some(found) = data::find_by_email(conn, &t, email).await? else { return Ok(true) };
    let own = match user_id {
        Some(id) => data::lock_account(conn, id).await?.and_then(|a| a.person_ci_id),
        None => None,
    };
    Ok(match own {
        Some(person) => found.id == person,
        None => found.linked_to.is_none(),
    })
}

/// Links every account with an e-mail that has no Person yet (the upgrade to
/// 0044; `shadoucmdb migrate` runs it after the reconcile that builds the
/// Person table, so it is a no-op once done). Each link is an `update` row on
/// the account. Returns how many were linked.
pub async fn link_all(conn: &mut PgConnection, ctx: &RequestContext) -> Result<usize, AppError> {
    let ids = data::unlinked_accounts(conn).await?;
    for id in &ids {
        let before = users::load(conn, *id).await?;
        link_user(conn, ctx, *id).await?;
        let after = users::load(conn, *id).await?;
        let entry = AuditEntry {
            action: AuditAction::Update,
            entity_type: "users",
            entity_id: *id,
            old_value: Some(crud::json(&before)),
            new_value: Some(crud::json(&after)),
        };
        crud::write_audit(conn, ctx, vec![entry]).await?;
    }
    Ok(ids.len())
}

// ---------------------------------------------------------------------------
// The sign-in account of a Person
// ---------------------------------------------------------------------------

/// The sign-in account a Person is linked to
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignInAccount {
    /// The account's id; null unless the caller holds users.manage (or the Administrator profile)
    #[schema(required = true)]
    pub user_id: Option<Uuid>,
    pub username: String,
    pub display_name: String,
    /// Disabled accounts keep their link
    pub is_active: bool,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignInAccountAnswer {
    /// Null when no account is linked to the configuration item. While one is,
    /// the Person's Email is managed by the account and the Person cannot be
    /// deleted or change its type.
    #[schema(required = true)]
    pub account: Option<SignInAccount>,
}

pub async fn sign_in_account(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<SignInAccountAnswer, AppError> {
    let mut conn = pool.acquire().await?;
    let Some(row) = crate::data::items::summary(&mut conn, id).await? else {
        return Err(AppError::missing("Configuration item", id));
    };
    ctx.require_class_visible(row.class_id, "Configuration item", id)?;
    let admin = match &ctx.caller {
        Caller::System => true,
        Caller::User(p) => p.permissions.has(GlobalPermission::UsersManage),
        Caller::Anonymous => false,
    };
    let account = data::linked_user(&mut conn, id).await?.map(|a| SignInAccount {
        user_id: admin.then_some(a.id),
        username: a.username,
        display_name: a.display_name,
        is_active: a.is_active,
    });
    Ok(SignInAccountAnswer { account })
}

pub fn routes() -> Vec<Route> {
    vec![
        route(Method::GET, "/api/v1/configuration-items/{id}/sign-in-account", "getSignInAccount")
            .tag("Configuration items")
            .summary("The sign-in account a Person is linked to")
            .description("For a CI of the built-in Person type: the user account linked to it (`account`, null when none is). While an account is linked, the Person's Email follows the account's e-mail and is read-only on the CI (409 CONFLICT, `managed_by_user`), and the Person cannot be deleted or change its type (409 CONFLICT, `person_linked`). Any CI the caller may view can be asked; other types have no account. `userId` is only shown to callers who may manage user accounts.")
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(sign_in_account(&api.pool, &api.ctx, id).await?))
            }),
    ]
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::http::{HeaderMap, header};
    use serde_json::{Value, json};
    use sqlx::Executor;

    use super::*;
    use crate::db::scratch;
    use crate::modules::api_tokens::tests::{Creds, app, call, code};
    use crate::modules::mfa::tests::{PASSWORD, setup};

    fn session(me: &Value, headers: &HeaderMap) -> Creds {
        let cookie = headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned())
            .filter(|c| !c.starts_with("shadoucmdb_mfa="))
            .collect::<Vec<_>>()
            .join("; ");
        Creds { cookie: Some(cookie), csrf: me["csrfToken"].as_str().map(str::to_owned), bearer: None }
    }

    async fn login(app: &Router, username: &str) -> (u16, Value, Creds) {
        let body = json!({ "username": username, "password": PASSWORD });
        let (status, v, headers) = call(app, "POST", "/api/v1/auth/login", &Creds::default(), Some(body)).await;
        let creds = session(&v, &headers);
        (status, v, creds)
    }

    async fn person_class(pool: &PgPool) -> Uuid {
        sqlx::query_scalar("SELECT id FROM ci_classes WHERE system_role = 'person'").fetch_one(pool).await.unwrap()
    }

    /// (label, email, deleted) of a Person CI.
    async fn person(pool: &PgPool, id: Uuid) -> (String, String, bool) {
        sqlx::query_as(
            "SELECT ci.label, p.email, ci.deleted_at IS NOT NULL FROM people.person p
             JOIN configuration_items ci ON ci.id = p.id WHERE p.id = $1",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn create_user(app: &Router, admin: &Creds, name: &str, email: &str) -> (u16, Value) {
        let body =
            json!({ "username": name, "displayName": format!("{name} Example"), "email": email, "password": PASSWORD });
        let (status, v, _) = call(app, "POST", "/api/v1/admin/users", admin, Some(body)).await;
        (status, v)
    }

    async fn create_person(app: &Router, admin: &Creds, class: Uuid, name: &str, email: &str) -> Value {
        let body = json!({ "classId": class, "attributes": { "name": name, "email": email } });
        let (status, v, _) = call(app, "POST", "/api/v1/configuration-items", admin, Some(body)).await;
        assert_eq!(status, 201, "{v}");
        v
    }

    fn person_id(user: &Value) -> Uuid {
        user["person"]["id"].as_str().unwrap().parse().unwrap()
    }

    /// Decisions 2 to 7: every account gets a Person (created or adopted), the
    /// account's e-mail is the source of truth, a linked Person cannot be
    /// deleted or retyped, and deleting the account leaves the Person.
    #[tokio::test]
    async fn accounts_are_linked_to_their_person() {
        let Some(db) = scratch::database("accounts_are_linked_to_their_person").await else { return };
        let (app, pool) = (app(db.pool.clone()), &db.pool);
        let (admin, me) = setup(&app).await;
        let class = person_class(pool).await;

        // First-run setup linked the administrator.
        assert_eq!(me["user"]["signInStatus"], "ready", "{me}");
        assert_eq!(person(pool, person_id(&me["user"])).await, ("Owner".into(), "owner@example.test".into(), false));

        // A new account gets a new Person: Name = display name.
        let (status, alice) = create_user(&app, &admin, "alice", "alice@example.test").await;
        assert_eq!(status, 201, "{alice}");
        let alice_person = person_id(&alice);
        assert_eq!(person(pool, alice_person).await, ("alice Example".into(), "alice@example.test".into(), false));
        assert_eq!(alice["person"]["label"], "alice Example");
        let created: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_log WHERE entity_type = 'configuration_items' AND action = 'create'
               AND entity_id = $1 AND actor_name = 'owner'",
        )
        .bind(alice_person)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(created, 1, "the auto-created Person is audited, naming the caller");

        // An existing Person without an account is adopted, whatever the case of its e-mail.
        let bob_ci = create_person(&app, &admin, class, "Bob Builder", "Bob@Example.test").await;
        let (status, bob) = create_user(&app, &admin, "bob", "bob@example.test").await;
        assert_eq!(status, 201, "{bob}");
        assert_eq!(bob["person"]["id"], bob_ci["id"]);
        assert_eq!(person(pool, person_id(&bob)).await, ("Bob Builder".into(), "bob@example.test".into(), false));

        // A deleted one too: it is restored.
        let gone = create_person(&app, &admin, class, "Carol", "carol@example.test").await;
        let gone_id: Uuid = gone["id"].as_str().unwrap().parse().unwrap();
        let (status, _, _) =
            call(&app, "DELETE", &format!("/api/v1/configuration-items/{gone_id}"), &admin, None).await;
        assert_eq!(status, 204);
        let (status, carol) = create_user(&app, &admin, "carol", "carol@example.test").await;
        assert_eq!((status, person_id(&carol)), (201, gone_id), "{carol}");
        assert!(!person(pool, gone_id).await.2, "restored");

        // E-mails are unique ignoring case, across accounts...
        let (status, v) = create_user(&app, &admin, "alice2", "ALICE@example.test").await;
        assert_eq!((status, code(&v)), (409, "CONFLICT"), "{v}");
        assert_eq!(v["error"]["details"][0]["field"], "email", "{v}");
        // ... and across Persons, in the database (any write path).
        let dup = format!(
            "INSERT INTO configuration_items (id, class_id, label) VALUES (gen_random_uuid(), '{class}', 'dup')
                           RETURNING id"
        );
        let dup_id: Uuid = sqlx::query_scalar(sqlx::AssertSqlSafe(dup)).fetch_one(pool).await.unwrap();
        let err = sqlx::query("INSERT INTO people.person (id, name, email) VALUES ($1, 'Dup', 'ALICE@EXAMPLE.TEST')")
            .bind(dup_id)
            .execute(pool)
            .await
            .unwrap_err();
        assert!(err.as_database_error().unwrap().constraint().unwrap().starts_with("uq_"), "{err}");
        let body = json!({ "classId": class, "attributes": { "name": "Dup", "email": "alice@EXAMPLE.test" } });
        let (status, v, _) = call(&app, "POST", "/api/v1/configuration-items", &admin, Some(body)).await;
        assert_eq!((status, &v["error"]["details"][0]["field"]), (409, &json!("attributes.email")), "{v}");

        // The account's e-mail is the source of truth: a change moves the Person's along.
        let alice_id = alice["id"].as_str().unwrap();
        let patch = json!({ "email": "alice.new@example.test" });
        let (status, v, _) = call(&app, "PATCH", &format!("/api/v1/admin/users/{alice_id}"), &admin, Some(patch)).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(person(pool, alice_person).await.1, "alice.new@example.test");
        // Refused while another Person has the new address; nothing changes.
        create_person(&app, &admin, class, "Dave", "dave@example.test").await;
        let patch = json!({ "email": "Dave@example.test" });
        let (status, v, _) = call(&app, "PATCH", &format!("/api/v1/admin/users/{alice_id}"), &admin, Some(patch)).await;
        assert_eq!((status, &v["error"]["details"][0]["code"]), (409, &json!("person_email_taken")), "{v}");
        let (_, v, _) = call(&app, "GET", &format!("/api/v1/admin/users/{alice_id}"), &admin, None).await;
        assert_eq!(v["email"], "alice.new@example.test");
        // It cannot be cleared.
        let patch = json!({ "email": null });
        let (status, _, _) = call(&app, "PATCH", &format!("/api/v1/admin/users/{alice_id}"), &admin, Some(patch)).await;
        assert_eq!(status, 400);

        // On the Person, the e-mail is read-only while it is linked (resending it is fine).
        let path = format!("/api/v1/configuration-items/{alice_person}");
        let patch = json!({ "attributes": { "email": "elsewhere@example.test" } });
        let (status, v, _) = call(&app, "PATCH", &path, &admin, Some(patch)).await;
        assert_eq!((status, &v["error"]["details"][0]["code"]), (409, &json!("managed_by_user")), "{v}");
        assert!(v["error"]["message"].as_str().unwrap().contains("\"alice\""), "{v}");
        let patch = json!({ "attributes": { "email": "alice.new@example.test", "department": "IT" } });
        let (status, v, _) = call(&app, "PATCH", &path, &admin, Some(patch)).await;
        assert_eq!(status, 200, "{v}");
        let (status, v, _) = call(&app, "GET", &format!("{path}/sign-in-account"), &admin, None).await;
        assert_eq!((status, &v["account"]["username"]), (200, &json!("alice")), "{v}");
        assert_eq!(v["account"]["userId"], json!(alice_id));

        // A linked Person is not deleted (API, and the database for any other path).
        let (status, v, _) = call(&app, "DELETE", &path, &admin, None).await;
        assert_eq!((status, &v["error"]["details"][0]["code"]), (409, &json!("person_linked")), "{v}");
        let err = sqlx::query("UPDATE configuration_items SET deleted_at = now() WHERE id = $1")
            .bind(alice_person)
            .execute(pool)
            .await
            .unwrap_err();
        assert_eq!(err.as_database_error().unwrap().constraint(), Some("configuration_items_person_linked"));

        // Deleting the account leaves the Person, unlinked: then it can go.
        let (status, v, _) = call(&app, "DELETE", &format!("/api/v1/admin/users/{alice_id}"), &admin, None).await;
        assert_eq!(status, 200, "{v}");
        assert!(!person(pool, alice_person).await.2);
        let (_, v, _) = call(&app, "GET", &format!("{path}/sign-in-account"), &admin, None).await;
        assert_eq!(v["account"], Value::Null);
        let (status, _, _) = call(&app, "DELETE", &path, &admin, None).await;
        assert_eq!(status, 204);

        // The database holds the link: an account with an e-mail needs a live Person.
        let err = pool
            .execute("INSERT INTO users (username, display_name, email, password_hash) VALUES ('x', 'x', 'x@example.test', '$argon2id$x')")
            .await
            .unwrap_err();
        assert_eq!(err.as_database_error().unwrap().constraint(), Some("users_person_link"));

        // The Person's key fields stay (decision 1).
        let email_field: Uuid =
            sqlx::query_scalar("SELECT id FROM ci_attribute_definitions WHERE system_role = 'person_email'")
                .fetch_one(pool)
                .await
                .unwrap();
        let path = format!("/api/v1/attribute-definitions/{email_field}");
        for patch in [json!({ "isActive": false }), json!({ "isRequired": false }), json!({ "dataType": "number" })] {
            let (status, v, _) = call(&app, "PATCH", &path, &admin, Some(patch.clone())).await;
            assert_eq!((status, &v["error"]["details"][0]["code"]), (409, &json!("system_attribute")), "{patch}: {v}");
        }
        let (status, _, _) = call(&app, "DELETE", &path, &admin, None).await;
        assert_eq!(status, 409);
        let (status, v, _) = call(&app, "PATCH", &path, &admin, Some(json!({ "label": "E-mail" }))).await;
        assert_eq!((status, &v["systemRole"]), (200, &json!("person_email")), "{v}");
        let class_path = format!("/api/v1/ci-classes/{class}");
        let (status, v, _) = call(&app, "DELETE", &class_path, &admin, None).await;
        assert_eq!((status, &v["error"]["details"][0]["code"]), (409, &json!("system_class")), "{v}");

        db.drop().await;
    }

    /// Decisions 8 and 9: an account with an e-mail but no Person cannot sign
    /// in (answered like a wrong password, audited with the reason) and has no
    /// live session; an account without an e-mail signs in and must enter one
    /// before anything else.
    #[tokio::test]
    async fn sign_in_needs_a_person_or_asks_for_the_email() {
        let Some(db) = scratch::database("sign_in_needs_a_person_or_asks_for_the_email").await else { return };
        let (app, pool) = (app(db.pool.clone()), &db.pool);
        let (admin, _) = setup(&app).await;
        let (status, erin) = create_user(&app, &admin, "erin", "erin@example.test").await;
        assert_eq!(status, 201, "{erin}");
        let (status, _, erin_session) = login(&app, "erin").await;
        assert_eq!(status, 200);

        // An account created before e-mails were required, with erin's password.
        let legacy: Uuid = sqlx::query_scalar(
            "INSERT INTO users (username, display_name, password_hash)
             SELECT 'legacy', 'Legacy User', password_hash FROM users WHERE username = 'erin' RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO user_permission_profiles (user_id, profile_id) SELECT $1, id FROM permission_profiles WHERE is_builtin")
            .bind(legacy)
            .execute(pool)
            .await
            .unwrap();
        let (_, v, _) = call(&app, "GET", "/api/v1/admin/users?signInStatus=email_required", &admin, None).await;
        assert_eq!(v["data"].as_array().unwrap().len(), 1, "{v}");
        assert_eq!(v["data"][0]["username"], "legacy");
        // API tokens are held to the same rules as sessions (GH#529).
        let builtin: Uuid =
            sqlx::query_scalar("SELECT id FROM permission_profiles WHERE is_builtin").fetch_one(pool).await.unwrap();
        let expires = (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339();
        let mut tokens = Vec::new();
        for owner in [legacy, erin["id"].as_str().unwrap().parse().unwrap()] {
            let body = json!({ "name": "script", "userId": owner, "profileId": builtin, "expiresAt": expires });
            let (status, v, _) = call(&app, "POST", "/api/v1/admin/api-tokens", &admin, Some(body)).await;
            assert_eq!(status, 201, "{v}");
            tokens.push(Creds { bearer: v["secret"].as_str().map(str::to_owned), ..Creds::default() });
        }
        let (legacy_token, erin_token) = (&tokens[0], &tokens[1]);
        let last_outcome = async || -> Option<String> {
            sqlx::query_scalar(
                "SELECT new_value ->> 'outcome' FROM audit_log WHERE action = 'token.use' ORDER BY chain_seq DESC LIMIT 1",
            )
            .fetch_one(pool)
            .await
            .unwrap()
        };
        let (status, v, _) = call(&app, "GET", "/api/v1/configuration-items", legacy_token, None).await;
        assert_eq!((status, code(&v)), (403, "EMAIL_REQUIRED"), "{v}");
        assert_eq!(last_outcome().await.as_deref(), Some("email_required"));
        let (status, _, _) = call(&app, "GET", "/api/v1/configuration-items", erin_token, None).await;
        assert_eq!(status, 200);

        let (status, me, creds) = login(&app, "legacy").await;
        assert_eq!((status, &me["emailRequired"]), (200, &json!(true)), "{me}");
        let (status, v, _) = call(&app, "GET", "/api/v1/admin/users", &creds, None).await;
        assert_eq!((status, code(&v)), (403, "EMAIL_REQUIRED"), "{v}");
        let (status, _, _) = call(&app, "GET", "/api/v1/auth/me", &creds, None).await;
        assert_eq!(status, 200);
        // Taken by another account: refused.
        let (status, v, _) =
            call(&app, "PUT", "/api/v1/auth/email", &creds, Some(json!({ "email": "ERIN@example.test" }))).await;
        assert_eq!((status, code(&v)), (409, "CONFLICT"), "{v}");
        let (status, me, _) =
            call(&app, "PUT", "/api/v1/auth/email", &creds, Some(json!({ "email": "legacy@example.test" }))).await;
        assert_eq!(
            (status, &me["emailRequired"], &me["user"]["signInStatus"]),
            (200, &json!(false), &json!("ready")),
            "{me}"
        );
        assert_eq!(person(pool, person_id(&me["user"])).await.0, "Legacy User");
        let (status, _, _) = call(&app, "GET", "/api/v1/admin/users", &creds, None).await;
        assert_eq!(status, 200, "the session goes on");
        let (status, _, _) = call(&app, "GET", "/api/v1/configuration-items", legacy_token, None).await;
        assert_eq!(status, 200, "and the token works");
        let (status, v, _) =
            call(&app, "PUT", "/api/v1/auth/email", &creds, Some(json!({ "email": "other@example.test" }))).await;
        assert_eq!((status, code(&v)), (409, "CONFLICT"), "set once; an administrator changes it: {v}");

        // An incomplete account (the state an interrupted upgrade could leave).
        let erin_id: Uuid = erin["id"].as_str().unwrap().parse().unwrap();
        let mut c = pool.acquire().await.unwrap();
        c.execute("ALTER TABLE users DISABLE TRIGGER users_person_link").await.unwrap();
        sqlx::query("UPDATE users SET person_ci_id = NULL WHERE id = $1").bind(erin_id).execute(&mut *c).await.unwrap();
        c.execute("ALTER TABLE users ENABLE TRIGGER users_person_link").await.unwrap();
        drop(c);
        let (_, v, _) = call(&app, "GET", &format!("/api/v1/admin/users/{erin_id}"), &admin, None).await;
        assert_eq!(v["signInStatus"], "person_missing", "{v}");
        let (status, _, _) = call(&app, "GET", "/api/v1/auth/me", &erin_session, None).await;
        assert_eq!(status, 401, "its session is no longer live");
        let (status, v, _) = login(&app, "erin").await;
        assert_eq!((status, v["error"]["message"].as_str()), (401, Some("Invalid username or password")), "{v}");
        let reason: Option<String> = sqlx::query_scalar(
            "SELECT new_value ->> 'reason' FROM audit_log WHERE action = 'login.failure' ORDER BY chain_seq DESC LIMIT 1",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(reason.as_deref(), Some(crate::modules::auth::ACCOUNT_INCOMPLETE));
        let (status, v, _) = call(&app, "GET", "/api/v1/configuration-items", erin_token, None).await;
        assert_eq!((status, code(&v)), (401, "UNAUTHENTICATED"), "{v}");
        assert_eq!(last_outcome().await.as_deref(), Some("account_incomplete"));
        // Saving its e-mail again links it.
        let patch = json!({ "email": "erin@example.test" });
        let (status, v, _) = call(&app, "PATCH", &format!("/api/v1/admin/users/{erin_id}"), &admin, Some(patch)).await;
        assert_eq!((status, &v["signInStatus"]), (200, &json!("ready")), "{v}");
        let (status, _, _) = login(&app, "erin").await;
        assert_eq!(status, 200);
        let (status, _, _) = call(&app, "GET", "/api/v1/configuration-items", erin_token, None).await;
        assert_eq!(status, 200);

        db.drop().await;
    }
}
