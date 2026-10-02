//! Layout templates in use: which template each class and CI uses, a class's default template, and a CI's
//! own layout (`/configuration-items/{id}/layout`).
//!
//! Templates themselves are part of the settings document (`layoutTemplates`, saved with PUT
//! /ui-settings). A CI's own layout lives in `ci_layout_overrides`: either another template or a layout of
//! its own. Changing one needs `customization.manage` and the edit right on the CI's class; reading the
//! layout a CI shows needs the view right, like the CI.

use std::collections::BTreeMap;

use axum::http::Method;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

use super::document::{self, Issue, UiLayout, UiSettingsDocument};
use super::{InUse, UiSettings, load, parse_stored, save_in};
use crate::api::context::{Count, RequestContext};
use crate::api::route::{Body, Check, IdPath, In, Json, KeyPath, NoBody, NoContent, NoPath, NoQuery, Route, route};
use crate::api::schemas::ts_opt;
use crate::auth::permissions::{ClassOp, GlobalPermission};
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::data::items as item_data;
use crate::data::ui_settings::{self as data, OverrideRow};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};

const TAG: &str = "UI settings";
pub const OVERRIDE_ENTITY: &str = "ci_layout_overrides";

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// Where the layout a CI shows comes from
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CiLayoutSource {
    /// The default template of the CI's class
    ClassDefault,
    /// Another template, chosen for this CI
    Template,
    /// A layout of the CI's own
    Custom,
}

/// The layout a CI's detail page and form show, and where it comes from
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CiLayout {
    pub ci_id: Uuid,
    pub class_key: String,
    #[schema(inline)]
    pub source: CiLayoutSource,
    /// The template shown (the class default or the one chosen for the CI); null for a custom layout
    #[schema(required = true)]
    pub template_key: Option<String>,
    #[schema(required = true)]
    pub template_name: Option<String>,
    /// The class's default template: what the CI shows after DELETE
    pub class_template_key: String,
    /// The layout to render. Attribute fields the class does not have are left out (see `issues`).
    pub layout: UiLayout,
    /// What the layout leaves out or flags, with paths below "layout"
    pub issues: Vec<Issue>,
    /// Version of the CI's own layout, to send back in PUT; null when the CI uses its class's default
    #[schema(required = true)]
    pub version: Option<i32>,
    /// When the CI's own layout was last saved; null without one
    #[serde(serialize_with = "ts_opt::serialize")]
    #[schema(required = true, value_type = Option<String>, format = DateTime)]
    pub updated_at: Option<DateTime<Utc>>,
    #[schema(required = true)]
    pub updated_by: Option<String>,
}

/// A CI's own layout: another template (`templateKey`) or a layout of its own (`layout`), not both
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CiLayoutUpdate {
    /// Show this template (`layoutTemplates[].key`) instead of the class's default
    #[serde(default)]
    #[schema(nullable = false, pattern = "^[a-z][a-z0-9_]{0,62}$")]
    pub template_key: Option<String>,
    /// Show this layout, for this CI only
    #[serde(default)]
    #[schema(nullable = false)]
    pub layout: Option<UiLayout>,
    /// The `version` you loaded, if the CI has its own layout: when it was saved in between, the request
    /// fails with 409 VERSION_CONFLICT. Leave it out to overwrite.
    #[serde(default)]
    #[schema(nullable = false, minimum = 1)]
    pub version: Option<i32>,
}

impl Check for CiLayoutUpdate {
    fn check(&self) -> Vec<FieldError> {
        let field = |field: &str, message: &str| FieldError {
            location: FieldLocation::Body,
            field: field.into(),
            message: message.into(),
            code: "custom".into(),
        };
        match (&self.template_key, &self.layout) {
            (Some(_), Some(_)) => vec![field("layout", "Send templateKey or layout, not both")],
            (None, None) => vec![field("templateKey", "Send templateKey or layout")],
            (None, Some(l)) => l.problems("layout"),
            (Some(_), None) => Vec::new(),
        }
    }
}

/// A class's default template
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassTemplateUpdate {
    /// The template (`layoutTemplates[].key`)
    #[schema(pattern = "^[a-z][a-z0-9_]{0,62}$")]
    pub template_key: String,
    /// The UI settings version you loaded (409 VERSION_CONFLICT when they were saved in between); leave it
    /// out to apply the change to whatever is current
    #[serde(default)]
    #[schema(nullable = false, minimum = 1)]
    pub version: Option<i32>,
}
impl Check for ClassTemplateUpdate {}

/// Who uses a template
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LayoutTemplateUsage {
    pub key: String,
    pub name: String,
    /// Classes that use it as their default, by key
    pub class_keys: Vec<String>,
    /// Live CIs that show it instead of their class's default; null when some of them are in classes you
    /// may not view
    #[schema(value_type = Option<i64>, required = true)]
    pub override_count: Count,
}

/// The default template of a class
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassTemplate {
    pub class_key: String,
    pub class_name: String,
    pub template_key: String,
    /// Whether the class has a layout entry (false: it uses Standard because it has none)
    pub explicit: bool,
}

/// Which template each class uses, and who uses each template
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LayoutTemplateUsages {
    /// Every template, in the order of the settings
    pub templates: Vec<LayoutTemplateUsage>,
    /// Every class, by name
    pub classes: Vec<ClassTemplate>,
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

async fn stored(conn: &mut PgConnection) -> Result<UiSettingsDocument, AppError> {
    Ok(parse_stored(&data::current(conn, false).await?.settings).returned())
}

pub async fn usage(pool: &PgPool, ctx: &RequestContext) -> Result<LayoutTemplateUsages, AppError> {
    let mut conn = pool.acquire().await?;
    let doc = stored(&mut conn).await?;
    let model = data::model(&mut conn).await?;
    let cis = data::template_use(&mut conn).await?;
    let mut by_template: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut classes: Vec<ClassTemplate> = model
        .class_names
        .iter()
        .map(|(key, name)| {
            let explicit = doc.layouts.iter().any(|l| &l.class_key == key);
            ClassTemplate {
                class_key: key.clone(),
                class_name: name.clone(),
                template_key: doc.class_template(key).to_owned(),
                explicit,
            }
        })
        .collect();
    classes.sort_by(|a, b| {
        a.class_name.to_lowercase().cmp(&b.class_name.to_lowercase()).then(a.class_key.cmp(&b.class_key))
    });
    for c in &classes {
        by_template.entry(c.template_key.as_str()).or_default().push(c.class_key.clone());
    }
    let templates = doc
        .layout_templates
        .iter()
        .map(|t| LayoutTemplateUsage {
            key: t.key.clone(),
            name: t.name.clone(),
            class_keys: by_template.get(t.key.as_str()).cloned().unwrap_or_default(),
            override_count: match cis.get(&t.key) {
                Some((n, class_ids)) => Count::scoped(ctx, class_ids, *n),
                None => Count::Exact(0),
            },
        })
        .collect();
    Ok(LayoutTemplateUsages { templates, classes })
}

pub async fn set_class_template(
    pool: &PgPool,
    ctx: &RequestContext,
    class_key: &str,
    body: &ClassTemplateUpdate,
) -> Result<UiSettings, AppError> {
    let mut tx = pool.begin().await?;
    let current = data::current(&mut tx, true).await?;
    let model = data::model(&mut tx).await?;
    if !model.classes.contains_key(class_key) {
        return Err(AppError::not_found(format!("CI class {class_key} not found")));
    }
    let mut doc = parse_stored(&current.settings).returned();
    if doc.template(&body.template_key).is_none() {
        return Err(AppError::field("templateKey", "No layout template has this key", "not_found"));
    }
    // Only the reference changes: the stored form, not the expanded one, so no layout is edited.
    let mut compact = parse_stored(&current.settings);
    compact.layout_templates = std::mem::take(&mut doc.layout_templates);
    match compact.layouts.iter_mut().find(|l| l.class_key == class_key) {
        Some(l) => l.template_key = Some(body.template_key.clone()),
        None => compact.layouts.push(
            serde_json::from_value(json!({ "classKey": class_key, "templateKey": body.template_key }))
                .map_err(|_| AppError::internal())?,
        ),
    }
    let comment = format!("Default layout template of class {class_key}: {}", body.template_key);
    save_in(&mut tx, ctx, body.version, &compact, Some(&comment), InUse::Refuse).await?;
    let out = load(&mut tx).await?;
    tx.commit().await?;
    Ok(out)
}

/// The CI (any state) with its class, after the view check: 404 when missing or not viewable.
async fn ci_class(conn: &mut PgConnection, ctx: &RequestContext, id: Uuid) -> Result<(Uuid, String), AppError> {
    let row = item_data::summary(conn, id).await?.ok_or_else(|| AppError::missing("Configuration item", id))?;
    ctx.require_class_visible(row.class_id, "Configuration item", id)?;
    Ok((row.class_id, row.class_key))
}

fn effective(
    id: Uuid,
    class_key: String,
    doc: &UiSettingsDocument,
    model: &data::Model,
    own: Option<OverrideRow>,
) -> CiLayout {
    let class_template_key = doc.class_template(&class_key).to_owned();
    let mut issues = Vec::new();
    let (source, template_key, layout) = match own.as_ref() {
        Some(OverrideRow { layout: Some(l), .. }) => {
            (CiLayoutSource::Custom, None, serde_json::from_value::<UiLayout>(l.clone()).unwrap_or_default())
        }
        Some(OverrideRow { template_key: Some(k), .. }) if doc.template(k).is_some() => {
            (CiLayoutSource::Template, Some(k.clone()), UiLayout::default())
        }
        Some(OverrideRow { template_key: Some(k), .. }) => {
            issues.push(Issue {
                path: "templateKey".into(),
                code: document::IssueCode::UnknownTemplate,
                message: format!("Layout template \"{k}\" no longer exists; the class's default is shown"),
            });
            (CiLayoutSource::ClassDefault, Some(class_template_key.clone()), UiLayout::default())
        }
        _ => (CiLayoutSource::ClassDefault, Some(class_template_key.clone()), UiLayout::default()),
    };
    let template = template_key.as_deref().and_then(|k| doc.template(k));
    let layout = match template {
        Some(t) => t.layout.clone(),
        None => layout,
    };
    let (layout, more) = document::resolve_layout(&layout, &class_key, model);
    issues.extend(more);
    CiLayout {
        ci_id: id,
        class_key,
        source,
        template_name: template.map(|t| t.name.clone()),
        template_key: template.map(|t| t.key.clone()),
        class_template_key,
        layout,
        issues,
        version: own.as_ref().map(|o| o.version),
        updated_at: own.as_ref().map(|o| o.updated_at),
        updated_by: own.and_then(|o| o.updated_by_name),
    }
}

async fn read(conn: &mut PgConnection, id: Uuid, class_key: String) -> Result<CiLayout, AppError> {
    let doc = stored(conn).await?;
    let model = data::model(conn).await?;
    let own = data::layout_override(conn, id, false).await?;
    Ok(effective(id, class_key, &doc, &model, own))
}

pub async fn get_ci_layout(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<CiLayout, AppError> {
    let mut conn = pool.acquire().await?;
    let (_, class_key) = ci_class(&mut conn, ctx, id).await?;
    read(&mut conn, id, class_key).await
}

/// Locks a live CI the caller may edit; 404 when missing, deleted or not viewable, 403 without edit.
async fn lock_editable(conn: &mut PgConnection, ctx: &RequestContext, id: Uuid) -> Result<String, AppError> {
    match item_data::lock(conn, id).await? {
        Some(row) if row.deleted_at.is_none() => {
            ctx.require_class_visible(row.class_id, "Configuration item", id)?;
            ctx.require_class(row.class_id, ClassOp::Edit)?;
        }
        _ => return Err(AppError::missing("Configuration item", id)),
    }
    Ok(ci_class(conn, ctx, id).await?.1)
}

fn override_json(o: &OverrideRow) -> Value {
    json!({ "ciId": o.ci_id, "templateKey": o.template_key, "layout": o.layout, "version": o.version })
}

pub async fn put_ci_layout(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    body: &CiLayoutUpdate,
) -> Result<CiLayout, AppError> {
    let mut tx = pool.begin().await?;
    let class_key = lock_editable(&mut tx, ctx, id).await?;
    let before = data::layout_override(&mut tx, id, true).await?;
    match (body.version, &before) {
        (Some(v), Some(b)) if v != b.version => {
            return Err(AppError::new(
                ErrorCode::VersionConflict,
                format!(
                    "The CI's layout was changed{} since you loaded it (now version {}). Reload and apply your changes again.",
                    b.updated_by_name.as_deref().map(|n| format!(" by {n}")).unwrap_or_default(),
                    b.version
                ),
            ));
        }
        (Some(_), None) => {
            return Err(AppError::new(
                ErrorCode::VersionConflict,
                "The CI's own layout was removed since you loaded it. Reload and apply your changes again.",
            ));
        }
        _ => {}
    }
    let layout = match (&body.template_key, &body.layout) {
        (Some(k), _) => {
            if stored(&mut tx).await?.template(k).is_none() {
                return Err(AppError::field("templateKey", "No layout template has this key", "not_found"));
            }
            None
        }
        (None, Some(l)) => Some(serde_json::to_value(l.clone().normalized()).map_err(|_| AppError::internal())?),
        (None, None) => return Err(AppError::field("templateKey", "Send templateKey or layout", "custom")),
    };
    let unchanged =
        before.as_ref().is_some_and(|b| b.template_key == body.template_key && b.layout.as_ref() == layout.as_ref());
    if !unchanged {
        let after = data::put_layout_override(&mut tx, ctx, id, body.template_key.as_deref(), layout.as_ref()).await?;
        let entry = AuditEntry {
            action: if before.is_some() { AuditAction::Update } else { AuditAction::Create },
            entity_type: OVERRIDE_ENTITY,
            entity_id: id,
            old_value: before.as_ref().map(override_json),
            new_value: Some(override_json(&after)),
        };
        crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    }
    let out = read(&mut tx, id, class_key).await?;
    tx.commit().await?;
    Ok(out)
}

pub async fn delete_ci_layout(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    lock_editable(&mut tx, ctx, id).await?;
    if let Some(before) = data::layout_override(&mut tx, id, true).await? {
        data::delete_layout_override(&mut tx, id).await?;
        let entry = AuditEntry {
            action: AuditAction::Delete,
            entity_type: OVERRIDE_ENTITY,
            entity_id: id,
            old_value: Some(override_json(&before)),
            new_value: None,
        };
        crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    }
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

const CI_LAYOUT: &str = "/api/v1/configuration-items/{id}/layout";

pub fn routes() -> Vec<Route> {
    vec![
        route(Method::GET, "/api/v1/ui-settings/layout-templates/usage", "getLayoutTemplateUsage")
            .tag(TAG)
            .summary("Which layout template each class uses, and who uses each template")
            .description(
                "Per template: the classes that use it as their default and the number of live CIs that show it \
                 instead of their class's default (null when some of those CIs are in classes you may not view). \
                 Per class: its default template. Templates are edited in the settings document (`layoutTemplates`, \
                 PUT /api/v1/ui-settings); one that a class or a live CI uses cannot be removed (409 CONFLICT).",
            )
            .requires(GlobalPermission::CustomizationManage)
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                Ok(Json(usage(&api.pool, &api.ctx).await?))
            }),
        route(Method::PUT, "/api/v1/ui-settings/class-layouts/{key}", "setClassLayoutTemplate")
            .tag(TAG)
            .summary("Set a class's default layout template (saved as a new UI settings version)")
            .description(
                "Every CI of the class shows the template, except CIs with a layout of their own. The same as \
                 changing `layouts[].templateKey` with PUT /api/v1/ui-settings; no template's layout changes. \
                 404 for a class that does not exist, 400 for a template that does not.",
            )
            .requires(GlobalPermission::CustomizationManage)
            .errors(&[ErrorCode::NotFound, ErrorCode::VersionConflict])
            .handle(
                |api, In(KeyPath(key), NoQuery, Body(b)): In<KeyPath, NoQuery, Body<ClassTemplateUpdate>>| async move {
                    Ok(Json(set_class_template(&api.pool, &api.ctx, &key, &b).await?))
                },
            ),
        route(Method::GET, CI_LAYOUT, "getConfigurationItemLayout")
            .tag("Configuration items")
            .summary("The layout a CI's detail page and form show, and where it comes from")
            .description(
                "One call gives the page its layout: the CI's own (`custom`), another template chosen for it \
                 (`template`) or its class's default template (`class_default`), with attribute fields the class \
                 does not have left out. Needs view on the CI's class (404 otherwise, as for the CI). Deleted CIs \
                 keep their layout.",
            )
            .errors(&[ErrorCode::NotFound])
            .class_checked()
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(get_ci_layout(&api.pool, &api.ctx, id).await?))
            }),
        route(Method::PUT, CI_LAYOUT, "setConfigurationItemLayout")
            .tag("Configuration items")
            .summary("Give a CI another template or a layout of its own")
            .description(
                "Send `templateKey` (a template of the UI settings) or `layout` (for this CI only), not both. \
                 Needs `customization.manage` and edit on the CI's class (403 otherwise; 404 for a CI that is \
                 missing, deleted or in a class you may not view). Audited (entity type ci_layout_overrides, the \
                 CI's id). Saving what the CI already has changes nothing.",
            )
            .requires(GlobalPermission::CustomizationManage)
            .errors(&[ErrorCode::NotFound, ErrorCode::VersionConflict])
            .class_checked()
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<CiLayoutUpdate>>| async move {
                Ok(Json(put_ci_layout(&api.pool, &api.ctx, id, &b).await?))
            }),
        route(Method::DELETE, CI_LAYOUT, "resetConfigurationItemLayout")
            .tag("Configuration items")
            .summary("Reset a CI to its class's default layout template")
            .description(
                "Removes the CI's own layout (audited); 204 also when it had none. Needs `customization.manage` \
                 and edit on the CI's class.",
            )
            .requires(GlobalPermission::CustomizationManage)
            .errors(&[ErrorCode::NotFound])
            .class_checked()
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                delete_ci_layout(&api.pool, &api.ctx, id).await?;
                Ok(NoContent)
            }),
    ]
}
