//! SQL for the link between sign-in accounts and Person CIs (SHAA-1505,
//! migration 0044).

use sqlx::{AssertSqlSafe, PgConnection};
use uuid::Uuid;

use crate::schema::model::{Field, Model, TableName};

/// The built-in Person type and its key fields, found by their system role.
#[derive(Debug, Clone)]
pub struct PersonType {
    pub class_id: Uuid,
    pub name: Field,
    pub email: Field,
    pub table: TableName,
}

/// The Person type in `model`; None before migration 0044.
pub async fn person_type(conn: &mut PgConnection, model: &Model) -> sqlx::Result<Option<PersonType>> {
    let class_id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM cmdb.ci_classes WHERE system_role = 'person'")
        .fetch_optional(&mut *conn)
        .await?;
    let Some(class_id) = class_id else { return Ok(None) };
    let field = |role: &str| model.own_fields(class_id).find(|f| f.system_role.as_deref() == Some(role)).cloned();
    Ok(match (field("person_name"), field("person_email"), model.table(class_id)) {
        (Some(name), Some(email), Some(table)) => Some(PersonType { class_id, name, email, table }),
        _ => None,
    })
}

/// Whether the type's table exists (a database migrated but not reconciled yet has none).
pub async fn table_exists(conn: &mut PgConnection, table: &TableName) -> sqlx::Result<bool> {
    sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL").bind(table.sql()).fetch_one(conn).await
}

/// A Person CI holding an e-mail address.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PersonMatch {
    pub id: Uuid,
    /// The username of the account linked to it, if any.
    pub linked_to: Option<String>,
}

/// The Person whose e-mail is `email` ignoring case (deleted ones included;
/// the unique index allows one), locked until the transaction ends.
pub async fn find_by_email(conn: &mut PgConnection, t: &PersonType, email: &str) -> sqlx::Result<Option<PersonMatch>> {
    sqlx::query_as(AssertSqlSafe(format!(
        "SELECT ci.id, (SELECT u.username FROM cmdb.users u WHERE u.person_ci_id = ci.id) AS linked_to
         FROM {} p JOIN cmdb.configuration_items ci ON ci.id = p.id
         WHERE lower(p.{}) = lower($1)
         FOR UPDATE OF ci",
        t.table.sql(),
        t.email.column()
    )))
    .persistent(false)
    .bind(email)
    .fetch_optional(conn)
    .await
}

/// The Person's stored e-mail (None: no row in the Person table).
pub async fn email_of(conn: &mut PgConnection, t: &PersonType, ci_id: Uuid) -> sqlx::Result<Option<String>> {
    let found: Option<Option<String>> =
        sqlx::query_scalar(AssertSqlSafe(format!("SELECT {} FROM {} WHERE id = $1", t.email.column(), t.table.sql())))
            .persistent(false)
            .bind(ci_id)
            .fetch_optional(conn)
            .await?;
    Ok(found.flatten())
}

/// What linking needs to know about an account, its row locked.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Account {
    pub username: String,
    pub display_name: String,
    pub email: Option<String>,
    pub person_ci_id: Option<Uuid>,
}

pub async fn lock_account(conn: &mut PgConnection, user_id: Uuid) -> sqlx::Result<Option<Account>> {
    sqlx::query_as("SELECT username, display_name, email, person_ci_id FROM cmdb.users WHERE id = $1 FOR UPDATE")
        .bind(user_id)
        .fetch_optional(conn)
        .await
}

pub async fn set_link(conn: &mut PgConnection, user_id: Uuid, person: Option<Uuid>) -> sqlx::Result<()> {
    sqlx::query("UPDATE cmdb.users SET person_ci_id = $2 WHERE id = $1")
        .bind(user_id)
        .bind(person)
        .execute(conn)
        .await?;
    Ok(())
}

/// Accounts with an e-mail that no Person is linked to yet (after the upgrade
/// to 0044), oldest first.
pub async fn unlinked_accounts(conn: &mut PgConnection) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar(
        "SELECT id FROM cmdb.users WHERE email IS NOT NULL AND person_ci_id IS NULL ORDER BY created_at, id",
    )
    .fetch_all(conn)
    .await
}

/// The account linked to a CI.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LinkedUser {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    pub email: Option<String>,
    pub is_active: bool,
}

pub async fn linked_user(conn: &mut PgConnection, ci_id: Uuid) -> sqlx::Result<Option<LinkedUser>> {
    sqlx::query_as("SELECT id, username, display_name, email, is_active FROM cmdb.users WHERE person_ci_id = $1")
        .bind(ci_id)
        .fetch_optional(conn)
        .await
}

/// Labels of these Person CIs.
pub async fn labels(conn: &mut PgConnection, ids: &[Uuid]) -> sqlx::Result<Vec<(Uuid, String)>> {
    sqlx::query_as("SELECT id, label FROM cmdb.configuration_items WHERE id = ANY($1)").bind(ids).fetch_all(conn).await
}

/// Whether an account other than `except` has this e-mail, ignoring case.
pub async fn email_taken(conn: &mut PgConnection, email: &str, except: Option<Uuid>) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM cmdb.users WHERE lower(email) = lower($1) AND id IS DISTINCT FROM $2)",
    )
    .bind(email)
    .bind(except)
    .fetch_one(conn)
    .await
}
