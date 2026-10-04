//! Request and response bodies of the bulk import API (SHAA-714 §3.1–3.2).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::analyse::{ColumnInfo, FileFormat, PreviewRow};
use super::parse::csv::Encoding;
use crate::api::route::Check;
use crate::api::schemas::QueryBool;

/// Where a job is (§3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[schema(as = ImportStatus)]
pub enum JobStatus {
    /// The file is still arriving (seen only when listing during an upload).
    Uploading,
    /// Waiting for a worker; `phase` says for what.
    Queued,
    Analysing,
    /// Analysed; the mapping can be set and the dry run started.
    Ready,
    Validating,
    /// The dry run finished.
    Validated,
    Committing,
    Completed,
    CompletedWithErrors,
    Failed,
    Cancelled,
    /// The file (and the row problems) were deleted after 24 h.
    Expired,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Uploading => "uploading",
            JobStatus::Queued => "queued",
            JobStatus::Analysing => "analysing",
            JobStatus::Ready => "ready",
            JobStatus::Validating => "validating",
            JobStatus::Validated => "validated",
            JobStatus::Committing => "committing",
            JobStatus::Completed => "completed",
            JobStatus::CompletedWithErrors => "completed_with_errors",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
            JobStatus::Expired => "expired",
        }
    }

    /// Nothing happens to the job any more.
    pub fn is_final(self) -> bool {
        matches!(
            self,
            JobStatus::Completed
                | JobStatus::CompletedWithErrors
                | JobStatus::Failed
                | JobStatus::Cancelled
                | JobStatus::Expired
        )
    }

    /// A worker phase is running or waiting, or the file is arriving.
    pub fn is_running(self) -> bool {
        matches!(
            self,
            JobStatus::Uploading
                | JobStatus::Queued
                | JobStatus::Analysing
                | JobStatus::Validating
                | JobStatus::Committing
        )
    }
}

/// What `queued` and `progress` refer to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[schema(as = ImportPhase)]
pub enum Phase {
    Analyse,
    Validate,
    Commit,
}

/// The uploaded file as the analysis found it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportFile {
    pub name: String,
    pub format: FileFormat,
    /// Bytes
    pub size: i64,
    /// SHA-256 (hex) of the file; null while it is uploading
    pub sha256: Option<String>,
    /// XLSX: the worksheets, in workbook order
    pub sheets: Vec<String>,
    /// XLSX: the hidden worksheets among `sheets`
    pub hidden_sheets: Vec<String>,
    /// XLSX: the worksheet read
    pub sheet: Option<String>,
    /// CSV: the encoding read
    pub encoding: Option<Encoding>,
    /// CSV: the delimiter read
    pub delimiter: Option<String>,
    pub has_header_row: bool,
    /// Data rows (null until analysed)
    pub row_count: Option<u32>,
    pub column_count: Option<u32>,
    /// The first 20 data rows as read, for the step 1 preview
    pub preview_rows: Vec<PreviewRow>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = ImportProgress)]
pub struct Progress {
    /// Rows done in the current phase
    pub done: i32,
    pub total: i32,
    /// Jobs ahead of this one while it is queued
    pub queue_position: Option<i64>,
    pub started_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

/// Counts of a commit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = ImportCommitCounts)]
pub struct CommitCounts {
    pub created: u32,
    pub updated: u32,
    pub unchanged: u32,
    /// Rows with dry-run errors that `skipErrorRows` left out
    pub skipped: u32,
    /// Rows that failed at commit
    pub failed: u32,
    pub relationships_added: u32,
    /// Workflow instances started on the created CIs by workflows that start on their own
    #[serde(default)]
    pub workflows_started: u32,
}

/// Counts of the dry run, and of the commit once it ran.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportSummary {
    pub create: u32,
    pub update: u32,
    pub unchanged: u32,
    pub error_rows: u32,
    pub warnings: u32,
    pub relationships_to_add: u32,
    /// Problems found, including those beyond the 10,000 stored
    pub issues_total: u32,
    pub committed: Option<CommitCounts>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ImportRowOutcome)]
pub enum RowOutcome {
    Create,
    Update,
    Unchanged,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = ImportFieldChange)]
pub struct FieldChange {
    /// `attributes.<key>`, `ident`, `validFrom`, `validUntil` or `relationships.<typeKey>`
    pub field: String,
    pub old: Value,
    pub new: Value,
}

/// A planned row in the dry run's sample (at most 50).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = ImportPlannedRow)]
pub struct PlannedRow {
    pub row: u32,
    pub outcome: RowOutcome,
    /// The matched CI (updates and unchanged rows)
    pub ci_id: Option<Uuid>,
    /// Its label, or the label the new CI will get
    pub ci_label: Option<String>,
    pub changes: Vec<FieldChange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ImportStaleReason)]
pub enum StaleReason {
    /// The data model changed after the dry run
    ModelChanged,
    /// The dry run is older than 24 hours
    Expired,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = ImportDryRun)]
pub struct DryRunInfo {
    pub finished_at: DateTime<Utc>,
    pub stale: bool,
    pub stale_reason: Option<StaleReason>,
}

/// Why a job stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = ImportJobError)]
pub struct JobError {
    pub code: String,
    pub message: String,
    /// The row it concerns (the header is row 1)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row: Option<u32>,
    /// The 0-based column it concerns
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = ImportOwner)]
pub struct JobOwner {
    /// Null once the user was deleted
    pub id: Option<Uuid>,
    pub name: String,
}

/// A bulk import job (§3.1).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportJob {
    pub id: Uuid,
    pub status: JobStatus,
    pub phase: Option<Phase>,
    pub file: ImportFile,
    /// The file's columns with three sample values each (empty until analysed)
    pub columns: Vec<ColumnInfo>,
    /// The target class (set with the mapping)
    pub class_key: Option<String>,
    pub mapping: Option<ImportMapping>,
    /// The saved mapping the mapping came from, if any (informational)
    pub mapping_id: Option<Uuid>,
    pub progress: Progress,
    /// Null before the first dry run
    pub summary: Option<ImportSummary>,
    /// The first 50 planned rows of the dry run
    pub preview: Vec<PlannedRow>,
    pub dry_run: Option<DryRunInfo>,
    pub error: Option<JobError>,
    pub created_at: DateTime<Utc>,
    pub created_by: JobOwner,
    /// When the file is deleted (24 h after the last activity or the end of the job)
    pub expires_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// When a stop of the running commit was requested. The job stays
    /// `committing` until the current chunk is written, then ends as
    /// `cancelled` with its final counts.
    pub cancel_requested_at: Option<DateTime<Utc>>,
}

/// A job in lists: without columns, mapping and preview.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportJobSummary {
    pub id: Uuid,
    pub status: JobStatus,
    pub phase: Option<Phase>,
    pub file_name: String,
    pub file_format: FileFormat,
    pub file_size: i64,
    pub row_count: Option<u32>,
    pub class_key: Option<String>,
    pub progress: Progress,
    pub summary: Option<ImportSummary>,
    pub error: Option<JobError>,
    pub created_at: DateTime<Utc>,
    pub created_by: JobOwner,
    pub expires_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// `GET /imports` filters.
#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ListImportsQuery {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    pub limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    pub offset: i64,
    /// Only jobs in this status
    #[param(inline)]
    pub status: Option<JobStatus>,
    /// Administrators: every user's jobs, not only their own
    #[param(required = false, schema_with = all_schema)]
    pub all: Option<QueryBool>,
}
crate::paged!(ListImportsQuery);

fn all_schema() -> utoipa::openapi::schema::Schema {
    utoipa::openapi::schema::ObjectBuilder::new()
        .schema_type(utoipa::openapi::schema::Type::String)
        .enum_values(Some(["true", "false"]))
        .default(Some("false".into()))
        .into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ImportIssueSeverity)]
pub enum IssueSeverity {
    Error,
    Warning,
}

/// A problem the dry run or the commit found in one row (§3.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportIssue {
    /// Row number in the file (the header is row 1)
    pub row: u32,
    /// 0-based column, when the problem is in one cell
    pub column: Option<u32>,
    /// The column's header
    pub header: Option<String>,
    /// The field concerned, e.g. `attributes.os`
    pub field: Option<String>,
    /// The cell, at most 200 characters
    pub value: Option<String>,
    pub severity: IssueSeverity,
    pub code: String,
    pub message: String,
}

/// `GET /imports/{id}/issues` filters.
#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ListImportIssuesQuery {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    pub limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    pub offset: i64,
    #[param(inline)]
    pub severity: Option<IssueSeverity>,
    /// Only problems with this code
    #[param(max_length = 64)]
    pub code: Option<String>,
    /// Only problems in this 0-based column
    #[param(minimum = 0, maximum = 199)]
    pub column: Option<u32>,
}
crate::paged!(ListImportIssuesQuery);

/// `PATCH /imports/{id}/file-options`: how to read the file; the analysis runs again.
pub type UpdateFileOptions = super::analyse::FileOptions;
impl Check for UpdateFileOptions {}

/// `POST /imports/{id}/commit`.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = CommitImport)]
pub struct CommitImport {
    /// Import the valid rows and leave out the rows the dry run found errors
    /// in. Without it, a dry run with error rows refuses the commit
    /// (`has_error_rows`).
    #[serde(default)]
    pub skip_error_rows: bool,
}
impl Check for CommitImport {}

// ---------------------------------------------------------------------------
// Mapping (§3.2): which file column goes where
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportMode {
    CreateOnly,
    UpdateOnly,
    CreateOrUpdate,
}

/// What an empty cell does to an existing CI (§2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ImportEmptyCells)]
pub enum EmptyCells {
    /// Leave the value unchanged (a new CI gets the attribute's default)
    Ignore,
    /// Clear the value
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = ImportDateFormat)]
pub enum DateFormat {
    #[serde(rename = "YYYY-MM-DD")]
    Iso,
    #[serde(rename = "DD.MM.YYYY")]
    DayMonthYear,
    #[serde(rename = "MM/DD/YYYY")]
    MonthDayYear,
}

/// How the other CI of a reference or relationship is found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ImportMatchBy)]
pub enum MatchBy {
    Ident,
    Label,
    /// A text, integer, IP or CIDR attribute of the target class
    Attribute,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = ImportTargetMatch)]
pub struct TargetMatch {
    pub by: MatchBy,
    /// Required with `by: attribute`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribute_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ImportRelationshipDirection)]
pub enum RelationshipDirection {
    /// This row's CI is the source
    Outgoing,
    /// This row's CI is the target
    Incoming,
}

/// Where a column goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = ImportColumnTarget)]
pub enum ColumnTarget {
    /// An attribute of the class or an ancestor, by key
    #[serde(rename_all = "camelCase")]
    Attribute {
        key: String,
        /// Required for reference attributes
        #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
        match_: Option<TargetMatch>,
    },
    /// A relationship type, by key, in one direction
    #[serde(rename_all = "camelCase")]
    Relationship {
        type_key: String,
        direction: RelationshipDirection,
        #[serde(rename = "match")]
        match_: TargetMatch,
    },
    Ident,
    ValidFrom,
    ValidUntil,
    Ignore,
}

/// Per-column overrides of the job's options.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = ImportColumnOptions)]
pub struct ColumnOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(pattern = "^[.,]$")]
    pub decimal_separator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_format: Option<DateFormat>,
    /// IANA time zone for date-time values without an offset
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
}

/// The job's options (§3.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = ImportMappingOptions)]
pub struct MappingOptions {
    /// Trim spaces around text values (default true)
    #[serde(default = "yes")]
    pub trim: bool,
    #[serde(default = "dot")]
    #[schema(pattern = "^[.,]$")]
    pub decimal_separator: String,
    #[serde(default = "iso")]
    pub date_format: DateFormat,
    /// IANA time zone for date-time values without an offset (default UTC)
    #[serde(default = "utc")]
    pub time_zone: String,
    /// Separates several relationship targets in one cell (default `;`)
    #[serde(default = "semicolon")]
    #[schema(min_length = 1, max_length = 1)]
    pub list_separator: String,
}

impl Default for MappingOptions {
    fn default() -> Self {
        MappingOptions {
            trim: true,
            decimal_separator: dot(),
            date_format: iso(),
            time_zone: utc(),
            list_separator: semicolon(),
        }
    }
}

fn yes() -> bool {
    true
}
fn dot() -> String {
    ".".into()
}
fn iso() -> DateFormat {
    DateFormat::Iso
}
fn utc() -> String {
    "UTC".into()
}
fn semicolon() -> String {
    ";".into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = ImportMatchKey)]
pub struct MatchKey {
    /// `ident` or `attributes.<key>` (a text, integer, IP or CIDR attribute)
    pub field: String,
}

/// One mapped column of the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = ImportColumnMapping)]
pub struct ColumnMapping {
    /// 0-based column of the file
    pub index: u32,
    pub target: ColumnTarget,
    /// Overrides the job's `emptyCells` for this column
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub empty_cells: Option<EmptyCells>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<ColumnOptions>,
}

/// The mapping of a job (`PUT /imports/{id}/mapping`). Columns not listed are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportMapping {
    /// The target class, by key
    pub class_key: String,
    pub mode: ImportMode,
    /// How rows find existing CIs; omitted for `create_only`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<MatchKey>,
    #[serde(default = "ignore")]
    pub empty_cells: EmptyCells,
    #[serde(default)]
    pub options: MappingOptions,
    pub columns: Vec<ColumnMapping>,
}

/// Checked by `mapping::resolve` against the file and the data model.
impl Check for ImportMapping {}

fn ignore() -> EmptyCells {
    EmptyCells::Ignore
}

/// `GET /imports/template` query.
#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct TemplateQuery {
    /// The class to import into, by key
    #[param(min_length = 1, max_length = 63)]
    pub class_key: String,
}
