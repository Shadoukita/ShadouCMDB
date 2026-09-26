//! PostgreSQL errors caused by the client (constraint and trigger violations,
//! bad literals) translated into the error envelope, with the API field they
//! concern.

use sqlx::postgres::PgDatabaseError;

use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};

/// Constraints (declarative and trigger-raised) mapped to the API field they concern.
const CONSTRAINT_FIELDS: &[(&str, &str)] = &[
    ("ci_classes_not_own_parent", "parentId"),
    ("ci_classes_no_cycle", "parentId"),
    ("locations_not_own_parent", "parentId"),
    ("locations_no_cycle", "parentId"),
    ("locations_type_valid", "locationType"),
    ("owners_email_format", "email"),
    ("owners_kind_valid", "kind"),
    ("owners_external_ref_unique", "externalRef"),
    ("ci_attribute_definitions_class_key_uq", "key"),
    ("ci_attribute_definitions_enum_values", "enumValues"),
    ("ci_attribute_definitions_reference_class", "referenceClassId"),
    ("ci_attribute_definitions_validation_object", "validation"),
    ("ci_attribute_definitions_data_type_valid", "dataType"),
    ("ci_attribute_definitions_lookup_list", "lookupListId"),
    ("ci_attribute_definitions_lookup_list_id_fkey", "lookupListId"),
    ("ci_attribute_definitions_default_value", "defaultValue"),
    ("ci_classes_color_format", "color"),
    ("lookup_lists_name_not_blank", "name"),
    ("lookup_list_values_name_not_blank", "name"),
    ("lookup_list_values_color_format", "color"),
    ("lookup_list_values_list_key_uq", "key"),
    ("lookup_list_values_list_id_fkey", "listId"),
    ("lookup_list_values_list_immutable", "listId"),
    ("configuration_items_name_not_blank", "name"),
    ("configuration_items_hostname_format", "hostname"),
    ("configuration_items_class_concrete", "classId"),
    ("configuration_items_class_active", "classId"),
    ("configuration_items_class_change_attributes", "classId"),
    ("ci_relationships_no_self_edge", "targetCiId"),
    ("ci_relationships_endpoint_rule", "relationshipTypeId"),
    ("ci_relationships_type_active", "relationshipTypeId"),
    ("ci_relationships_live_endpoints", "sourceCiId"),
    ("ci_relationships_live_edge_uq", "targetCiId"),
    ("relationship_type_rules_uq", "targetClassId"),
    ("users_username_uq", "username"),
    ("users_username_format", "username"),
    ("users_display_name_not_blank", "displayName"),
    ("users_email_format", "email"),
    ("permission_profiles_name_uq", "name"),
    ("permission_profiles_name_not_blank", "name"),
    ("permission_profile_class_permissions_class_id_fkey", "classPermissions"),
    ("user_permission_profiles_profile_id_fkey", "profileIds"),
];

fn snake_to_camel(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut upper = false;
    for c in s.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn field_for(pg: &PgDatabaseError) -> String {
    if let Some(c) = pg.constraint() {
        if let Some((_, f)) = CONSTRAINT_FIELDS.iter().find(|(k, _)| *k == c) {
            return (*f).to_owned();
        }
        if c.ends_with("_key_format") || c.ends_with("_key_unique") || c.ends_with("_key_key") {
            return "key".to_owned();
        }
    }
    if let Some(col) = pg.column() {
        return snake_to_camel(col);
    }
    // "Key (status_id)=(...) is not present in table ..." / "Key (key)=(x) already exists."
    if let Some(rest) = pg.detail().and_then(|d| d.strip_prefix("Key ("))
        && let Some(end) = rest.find(')')
        && rest[..end].bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
    {
        return snake_to_camel(&rest[..end]);
    }
    "(root)".to_owned()
}

/// Trigger messages are "table: human text"; drop the table prefix.
fn humanise(msg: &str) -> String {
    match msg.split_once(": ") {
        Some((prefix, rest)) if !prefix.is_empty() && prefix.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') => {
            rest.to_owned()
        }
        _ => msg.to_owned(),
    }
}

fn pg_error(err: &sqlx::Error) -> Option<&PgDatabaseError> {
    err.as_database_error()?.try_downcast_ref::<PgDatabaseError>()
}

/// Translate a PostgreSQL error into an AppError, or None when it is not a
/// client-caused error. `field_prefix` scopes errors raised while writing a
/// nested value (e.g. "attributes.cpu_cores").
pub fn map(err: &sqlx::Error, field_prefix: Option<&str>) -> Option<AppError> {
    let pg = pg_error(err)?;
    let field = field_prefix.map(str::to_owned).unwrap_or_else(|| field_for(pg));
    match pg.constraint() {
        Some("users_last_administrator") => {
            return Some(AppError::new(ErrorCode::LastAdministrator, humanise(pg.message())));
        }
        Some("permission_profiles_builtin_protected") => return Some(AppError::conflict(humanise(pg.message()))),
        _ => {}
    }
    match pg.code() {
        "23505" => {
            Some(AppError::new(ErrorCode::Conflict, humanise(pg.detail().unwrap_or(pg.message()))).with_details(vec![
                FieldError {
                    location: FieldLocation::Body,
                    field,
                    message: "Already exists".into(),
                    code: "unique".into(),
                },
            ]))
        }
        // restrict_violation (ON DELETE RESTRICT) and foreign-key violations
        code @ ("23001" | "23503") => {
            if code == "23001" || pg.detail().is_some_and(|d| d.contains("is still referenced")) {
                Some(AppError::new(
                    ErrorCode::InUse,
                    "This record is still referenced by other records. Retire it with isActive=false instead of deleting it.",
                ))
            } else {
                Some(AppError::field(field, "Referenced record does not exist", "not_found"))
            }
        }
        "23514" => Some(AppError::field(field, humanise(pg.message()), pg.constraint().unwrap_or("check_violation"))),
        "23502" => Some(AppError::field(field, "Required", "required")),
        // bad inet/uuid/date literal, datetime overflow, numeric out of range, string too long
        "22P02" | "22007" | "22008" | "22003" | "22001" => Some(AppError::field(field, pg.message(), "invalid_format")),
        _ => None,
    }
}

/// Connection-level failures: the database is unreachable or went away.
pub fn is_connection_error(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Io(_) | sqlx::Error::Tls(_) | sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => true,
        _ => pg_error(err).is_some_and(|pg| pg.code().starts_with("08") || pg.code() == "57P01"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(snake_to_camel("status_id"), "statusId");
        assert_eq!(humanise("ci_relationships: runs_on is not allowed"), "runs_on is not allowed");
        assert_eq!(humanise("Key (key)=(x) already exists."), "Key (key)=(x) already exists.");
    }
}
