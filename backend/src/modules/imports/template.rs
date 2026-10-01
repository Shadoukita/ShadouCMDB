//! `GET /imports/template`: an empty CSV with the column names of a class
//! (§1.2 step 1, §5.1). Written by the server through [`csv_safe`], because
//! attribute labels are editable by anyone with `datamodel.manage`: a label
//! such as `=HYPERLINK(…)` must come out neutralised (CR5b).

use sqlx::PgPool;
use uuid::Uuid;

use super::jobs::require_enabled;
use crate::api::context::RequestContext;
use crate::auth::permissions::ClassOp;
use crate::config::ImportConfig;
use crate::data::classes as class_data;
use crate::http::error::AppError;
use crate::modules::csv_safe;

/// A class the caller may import into: concrete, active, visible, and with
/// create or edit on it. Anything else is `None`, reported like an unknown key.
pub async fn importable_class(
    conn: &mut sqlx::PgConnection,
    ctx: &RequestContext,
    key: &str,
) -> sqlx::Result<Option<Uuid>> {
    let row: Option<(Uuid, bool, bool)> =
        sqlx::query_as("SELECT id, is_abstract, is_active FROM cmdb.ci_classes WHERE key = $1")
            .bind(key)
            .fetch_optional(conn)
            .await?;
    Ok(row.and_then(|(id, is_abstract, is_active)| {
        let allowed = ctx.require_class_visible(id, "CI class", id).is_ok()
            && (ctx.require_class(id, ClassOp::Create).is_ok() || ctx.require_class(id, ClassOp::Edit).is_ok());
        (allowed && !is_abstract && is_active).then_some(id)
    }))
}

pub fn unknown_class(key: &str) -> AppError {
    AppError::not_found(format!("CI class \"{key}\" does not exist or cannot be imported into"))
}

/// The template's file name and content: `Ident`, then the labels of the
/// class's active attributes (its own and inherited), in form order.
pub async fn template(
    pool: &PgPool,
    ctx: &RequestContext,
    cfg: &ImportConfig,
    class_key: &str,
) -> Result<(String, String), AppError> {
    let mut conn = pool.acquire().await?;
    require_enabled(&mut conn, cfg).await?;
    let class_id = importable_class(&mut conn, ctx, class_key).await?.ok_or_else(|| unknown_class(class_key))?;
    let defs = class_data::effective_attributes(&mut conn, class_id).await?;
    let mut out = String::from(csv_safe::BOM);
    let headers = std::iter::once("Ident").chain(defs.iter().filter(|d| d.is_active).map(|d| d.label.as_str()));
    csv_safe::write_record(&mut out, ',', headers);
    Ok((format!("{class_key}-template.csv"), out))
}
