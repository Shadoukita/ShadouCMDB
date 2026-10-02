//! `GET /imports/{id}/mapping-suggestion`: a first mapping for a file (§3.3).
//!
//! Headers and target names are compared normalised (NFKC, lower case,
//! trimmed, runs of spaces, `_`, `-` and `.` as one space). Per column, in
//! file order: the saved mapping's target for the header, else an attribute
//! key (or `ident`, `valid from`, `valid until`), else exactly one attribute
//! label or relationship label. A target goes to the first column only. Only
//! targets the caller may use are offered: active attributes of a class they
//! may import into, and relationship types whose other end they can view.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use unicode_normalization::UnicodeNormalization;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::jobs::{check_owner, fetch, invalid_state, require_enabled};
use super::mapping::{matchable, other_end, rules_of};
use super::saved::{self, MappingDefinition};
use super::schemas::{
    ColumnMapping, ColumnTarget, EmptyCells, ImportMapping, ImportMode, JobStatus, MappingOptions, MatchBy, MatchKey,
    RelationshipDirection, TargetMatch,
};
use super::template::importable_class;
use crate::api::context::RequestContext;
use crate::config::ImportConfig;
use crate::data::classes as class_data;
use crate::http::error::{AppError, FieldError, FieldLocation};
use crate::modules::classes::AttributeDataType;
use crate::schema::model::Model;

/// A header or target name as compared (§3.3).
pub fn normalise(s: &str) -> String {
    let lower: String = s.nfkc().collect::<String>().to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut gap = false;
    for ch in lower.trim().chars() {
        if ch.is_whitespace() || matches!(ch, '_' | '-' | '.') {
            gap = true;
        } else {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(ch);
        }
    }
    out
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct SuggestQuery {
    /// The class to import into, by key
    #[param(min_length = 1, max_length = 63)]
    pub class_key: String,
    /// Start from this saved mapping (of the same class)
    pub mapping_id: Option<Uuid>,
}

/// Why a column got its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ImportMatchVia)]
pub enum MatchVia {
    /// The header is an attribute key, `ident`, `valid from` or `valid until`
    Key,
    /// The header is the label of exactly one attribute or relationship
    Label,
    /// The saved mapping names the header
    SavedMapping,
}

/// How one column of the file was matched.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = ImportColumnMatch)]
pub struct ColumnMatch {
    /// 0-based column of the file
    pub column: u32,
    /// Null when the column is left unmapped
    pub via: Option<MatchVia>,
    /// Why an unmapped column was left out: `ambiguous_label` (several targets have that label),
    /// `duplicate_target` (an earlier column took the target) or `ident_admin_only`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// The saved mapping a suggestion started from.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = ImportSavedMappingRef)]
pub struct SavedMappingRef {
    pub id: Uuid,
    pub name: String,
    /// True when it was picked because its headers equal the file's, not asked for with `mappingId`
    pub by_headers: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportMappingSuggestion {
    /// Ready for `PUT /imports/{id}/mapping`; check it before sending
    pub mapping: ImportMapping,
    /// One entry per column of the file
    pub matched_by: Vec<ColumnMatch>,
    pub saved_mapping: Option<SavedMappingRef>,
}

fn query_error(field: &str, message: String, code: &str) -> AppError {
    AppError::validation(vec![FieldError {
        location: FieldLocation::Query,
        field: field.into(),
        message,
        code: code.into(),
    }])
}

/// A target and the name that makes it unique in one mapping.
#[derive(Debug, Clone)]
struct Candidate {
    id: String,
    target: ColumnTarget,
}

fn label_match() -> TargetMatch {
    TargetMatch { by: MatchBy::Label, attribute_key: None }
}

pub async fn suggest(
    pool: &PgPool,
    ctx: &RequestContext,
    cfg: &ImportConfig,
    id: Uuid,
    q: &SuggestQuery,
) -> Result<ImportMappingSuggestion, AppError> {
    let mut conn = pool.acquire().await?;
    require_enabled(&mut conn, cfg).await?;
    let job = check_owner(ctx, fetch(&mut conn, id).await?, id)?;
    if !matches!(job.status, JobStatus::Ready | JobStatus::Validated) {
        return Err(invalid_state(
            "A mapping can be suggested only after the file was analysed and while no step runs.",
        ));
    }
    let headers: Vec<String> =
        job.info().map(|i| i.columns.into_iter().map(|c| c.header).collect()).unwrap_or_default();
    let class_id = importable_class(&mut conn, ctx, &q.class_key).await?.ok_or_else(|| {
        query_error(
            "classKey",
            format!("CI class \"{}\" does not exist or cannot be imported into", q.class_key),
            "unknown_class",
        )
    })?;

    // The saved mapping: asked for, or the only one of the class with the file's headers.
    let saved = match q.mapping_id {
        Some(mid) => {
            let m = saved::fetch_visible(&mut conn, ctx, mid).await?;
            if m.class_key != q.class_key {
                return Err(query_error(
                    "mappingId",
                    "This saved mapping is for another class".into(),
                    "mapping_class_mismatch",
                ));
            }
            Some((m, false))
        }
        None => {
            let file_set: HashSet<String> = headers.iter().map(|h| normalise(h)).filter(|h| !h.is_empty()).collect();
            let mut same: Vec<_> = saved::visible_of(&mut conn, ctx, Some(&q.class_key))
                .await?
                .into_iter()
                .filter(|m| m.definition.header_set() == file_set)
                .collect();
            if same.len() == 1 { same.pop().map(|m| (m, true)) } else { None }
        }
    };

    let model = Model::load(&mut conn).await?;
    let defs = class_data::effective_attributes(&mut conn, class_id).await?;
    let admin = ctx.principal().is_some_and(|p| p.permissions.administrator);

    // What headers can name: by key (one target each) and by label (maybe several).
    let mut by_key: HashMap<String, Candidate> = HashMap::new();
    let mut by_label: HashMap<String, Vec<Candidate>> = HashMap::new();
    for (names, id, target) in [
        (vec!["ident"], "ident", ColumnTarget::Ident),
        (vec!["valid from", "validfrom"], "validFrom", ColumnTarget::ValidFrom),
        (vec!["valid until", "validuntil"], "validUntil", ColumnTarget::ValidUntil),
    ] {
        for n in names {
            by_key.insert(n.into(), Candidate { id: id.into(), target: target.clone() });
        }
    }
    for d in defs.iter().filter(|d| d.is_active) {
        let match_ = (d.data_type == AttributeDataType::Reference).then(label_match);
        let c = Candidate {
            id: format!("attributes.{}", d.key),
            target: ColumnTarget::Attribute { key: d.key.clone(), match_ },
        };
        by_key.entry(normalise(&d.key)).or_insert_with(|| c.clone());
        by_label.entry(normalise(&d.label)).or_default().push(c);
    }
    let types: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT key, forward_label, reverse_label FROM cmdb.relationship_types
         WHERE is_active AND system_role IS NULL ORDER BY sort_order, key",
    )
    .fetch_all(&mut *conn)
    .await?;
    for (key, forward, reverse) in types {
        let rules = rules_of(&mut conn, &key).await?;
        let mut labels: Vec<String> = Vec::new();
        for (direction, label) in
            [(RelationshipDirection::Outgoing, forward), (RelationshipDirection::Incoming, reverse)]
        {
            let label = normalise(&label);
            // A type with one label for both ends is offered once.
            if labels.contains(&label) || other_end(ctx, &model, &rules, class_id, direction).is_empty() {
                continue;
            }
            labels.push(label.clone());
            by_label.entry(label).or_default().push(Candidate {
                id: format!("relationships.{key}.{direction:?}"),
                target: ColumnTarget::Relationship { type_key: key.clone(), direction, match_: label_match() },
            });
        }
    }

    let definition: Option<&MappingDefinition> = saved.as_ref().map(|(m, _)| &m.definition);
    let mode = definition.map(|d| d.mode).unwrap_or(ImportMode::CreateOrUpdate);
    let mut used: HashSet<String> = HashSet::new();
    let mut columns: Vec<ColumnMapping> = Vec::new();
    let mut matched_by: Vec<ColumnMatch> = Vec::new();
    for (i, header) in headers.iter().enumerate() {
        let mut m = ColumnMatch { column: i as u32, via: None, hint: None };
        let n = normalise(header);
        let from_saved = definition.and_then(|d| d.target_for(header));
        let found: Option<(Candidate, MatchVia, Option<EmptyCells>, _)> = if let Some(c) = from_saved {
            let id = target_id(&c.target);
            Some((Candidate { id, target: c.target.clone() }, MatchVia::SavedMapping, c.empty_cells, c.options.clone()))
        } else if n.is_empty() {
            None
        } else if let Some(c) = by_key.get(&n) {
            Some((c.clone(), MatchVia::Key, None, None))
        } else {
            match by_label.get(&n).map(Vec::as_slice) {
                Some([one]) => Some((one.clone(), MatchVia::Label, None, None)),
                Some([_, _, ..]) => {
                    m.hint = Some("ambiguous_label".into());
                    None
                }
                _ => None,
            }
        };
        if let Some((c, via, empty_cells, options)) = found {
            let ident_refused = matches!(c.target, ColumnTarget::Ident) && !admin && mode != ImportMode::UpdateOnly;
            if ident_refused && via != MatchVia::SavedMapping {
                m.hint = Some("ident_admin_only".into());
            } else if !matches!(c.target, ColumnTarget::Ignore) && !used.insert(c.id.clone()) {
                m.hint = Some("duplicate_target".into());
            } else {
                m.via = Some(via);
                columns.push(ColumnMapping { index: i as u32, target: c.target, empty_cells, options });
            }
        }
        matched_by.push(m);
    }

    let mapping = match definition {
        Some(d) => ImportMapping {
            class_key: q.class_key.clone(),
            mode: d.mode,
            key: d.key.clone(),
            empty_cells: d.empty_cells,
            options: d.options.clone(),
            columns,
        },
        None => {
            let key = suggested_key(&model, class_id, &defs, &columns);
            ImportMapping {
                class_key: q.class_key.clone(),
                mode: if key.is_some() { ImportMode::CreateOrUpdate } else { ImportMode::CreateOnly },
                key,
                empty_cells: EmptyCells::Ignore,
                options: MappingOptions::default(),
                columns,
            }
        }
    };
    Ok(ImportMappingSuggestion {
        mapping,
        matched_by,
        saved_mapping: saved.map(|(m, by_headers)| SavedMappingRef { id: m.id, name: m.name, by_headers }),
    })
}

/// The name a target is unique by, as in `mapping::resolve`.
fn target_id(t: &ColumnTarget) -> String {
    match t {
        ColumnTarget::Attribute { key, .. } => format!("attributes.{key}"),
        ColumnTarget::Relationship { type_key, direction, .. } => format!("relationships.{type_key}.{direction:?}"),
        ColumnTarget::Ident => "ident".into(),
        ColumnTarget::ValidFrom => "validFrom".into(),
        ColumnTarget::ValidUntil => "validUntil".into(),
        ColumnTarget::Ignore => "ignore".into(),
    }
}

/// How rows should find existing CIs: the ident when mapped, else the
/// class's title attribute, else the first required matchable attribute
/// mapped. None means the file can only create.
fn suggested_key(
    model: &Model,
    class_id: Uuid,
    defs: &[class_data::EffectiveAttributeRow],
    columns: &[ColumnMapping],
) -> Option<MatchKey> {
    let mapped =
        |key: &str| columns.iter().any(|c| matches!(&c.target, ColumnTarget::Attribute { key: k, .. } if k == key));
    if columns.iter().any(|c| matches!(c.target, ColumnTarget::Ident)) {
        return Some(MatchKey { field: "ident".into() });
    }
    let title = model.title_field(class_id).map(|f| f.key.clone());
    let pick = defs
        .iter()
        .filter(|d| d.is_active && matchable(d.data_type) && mapped(&d.key))
        .find(|d| Some(&d.key) == title.as_ref())
        .or_else(|| defs.iter().find(|d| d.is_active && d.is_required && matchable(d.data_type) && mapped(&d.key)))?;
    Some(MatchKey { field: format!("attributes.{}", pick.key) })
}

#[cfg(test)]
mod tests {
    use super::normalise;

    #[test]
    fn headers_are_compared_normalised() {
        assert_eq!(normalise("  Host_Name "), "host name");
        assert_eq!(normalise("IP-Address"), "ip address");
        assert_eq!(normalise("valid.__from"), "valid from");
        assert_eq!(normalise("ＨＯＳＴ"), "host"); // NFKC folds full-width letters
        assert_eq!(normalise("Straße"), "straße");
        assert_eq!(normalise("_-."), "");
        assert_eq!(normalise("a  \t b"), "a b");
    }
}
