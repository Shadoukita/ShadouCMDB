//! The tables holding values sealed with the [`Keyring`], and what `serve`
//! does with them before it accepts requests: refuse to start when rows are
//! encrypted with a key that is not configured, then encrypt rows written
//! before encryption existed and re-encrypt rows under the previous key, in
//! one transaction under an advisory lock (several instances starting at once
//! do the work once). Backups, `restore` and `verify` report per table from
//! the same list.

use std::collections::BTreeMap;

use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{KeyId, Keyring, OpenError, Purpose, Sealed, Secret};

/// A table with sealed values. A new one is a variant here and an arm in each
/// method; the start-up routine, the backup header and `verify` follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SealedTable {
    /// TOTP seeds (`secret`, `key_id`).
    UserTotp,
    /// OIDC client secrets and LDAP bind passwords (`client_secret_enc`,
    /// `bind_password_enc`, `secrets_key_id`).
    IdentityProviders,
}

pub const SEALED_TABLES: &[SealedTable] = &[SealedTable::UserTotp, SealedTable::IdentityProviders];

impl SealedTable {
    /// Table name in the `cmdb` schema, as in the backup header.
    pub fn name(self) -> &'static str {
        match self {
            SealedTable::UserTotp => "user_totp",
            SealedTable::IdentityProviders => "identity_providers",
        }
    }

    pub fn from_name(name: &str) -> Option<SealedTable> {
        SEALED_TABLES.iter().copied().find(|t| t.name() == name)
    }

    fn key_column(self) -> &'static str {
        match self {
            SealedTable::UserTotp => "key_id",
            SealedTable::IdentityProviders => "secrets_key_id",
        }
    }

    /// The rows not encrypted yet; `migrated`: the key column exists.
    fn unencrypted_filter(self, migrated: bool) -> &'static str {
        match (self, migrated) {
            (SealedTable::UserTotp, true) => "key_id IS NULL",
            (SealedTable::UserTotp, false) => "true",
            // A provider without a secret has nothing to encrypt.
            (SealedTable::IdentityProviders, true) => {
                "secrets_key_id IS NULL AND (client_secret IS NOT NULL OR bind_password IS NOT NULL)"
            }
            (SealedTable::IdentityProviders, false) => "client_secret IS NOT NULL OR bind_password IS NOT NULL",
        }
    }

    /// `n` of what the table holds, for messages: "42 authenticator secrets".
    pub fn describe(self, n: i64) -> String {
        let (one, many) = match self {
            SealedTable::UserTotp => ("authenticator secret", "authenticator secrets"),
            SealedTable::IdentityProviders => ("identity provider secret", "identity provider secrets"),
        };
        format!("{n} {}", if n == 1 { one } else { many })
    }

    /// The command that gives up rows under a lost key, and what it does.
    fn reset(self) -> (&'static str, &'static str) {
        match self {
            SealedTable::UserTotp => (
                "shadoucmdb mfa reset-undecryptable",
                "turns off two-factor sign-in for those users so they can enrol again",
            ),
            SealedTable::IdentityProviders => (
                "shadoucmdb identity-providers reset-undecryptable",
                "disables those providers and clears their secrets until an administrator enters them again",
            ),
        }
    }

    /// Encrypts the rows written before encryption and re-encrypts those under
    /// the previous key. Rows under any other key are left alone: [`prepare`]
    /// has refused to start before this runs.
    async fn rewrap(self, conn: &mut PgConnection, keyring: &Keyring) -> Result<Rewrapped, PrepareError> {
        let mut done = Rewrapped { table: self, ..Rewrapped::default() };
        match self {
            SealedTable::UserTotp => {
                let rows: Vec<(Uuid, Vec<u8>, Option<i32>)> = sqlx::query_as(
                    "SELECT user_id, secret, key_id FROM user_totp WHERE key_id IS DISTINCT FROM $1 FOR UPDATE",
                )
                .bind(keyring.active_id().0)
                .fetch_all(&mut *conn)
                .await?;
                for (user_id, stored, key_id) in rows {
                    let seed = match open_totp_secret_to_rewrap(keyring, user_id, key_id, &stored) {
                        Ok(seed) => seed,
                        Err(OpenError::UnknownKey(k)) => {
                            return Err(PrepareError::Refused(format!(
                                "user_totp row of user {user_id} is encrypted with key {k}, which is not configured"
                            )));
                        }
                        Err(OpenError::Invalid) => {
                            // Left as it is: sign-in with its codes fails closed, and an
                            // administrator resets the user's two-factor authentication.
                            tracing::error!(
                                user_id = %user_id,
                                key = ?key_id.map(KeyId),
                                "the authenticator secret of this user does not decrypt (altered or copied from \
                                 another row); it cannot be re-encrypted. Reset the user's two-factor \
                                 authentication so they can enrol again"
                            );
                            done.failed += 1;
                            continue;
                        }
                    };
                    let sealed = seal_totp_secret(keyring, user_id, &seed);
                    sqlx::query("UPDATE user_totp SET secret = $1, key_id = $2 WHERE user_id = $3")
                        .bind(&sealed.bytes)
                        .bind(sealed.key_id.0)
                        .bind(user_id)
                        .execute(&mut *conn)
                        .await?;
                    if key_id.is_none() { done.unencrypted += 1 } else { done.from_previous += 1 }
                }
            }
            SealedTable::IdentityProviders => {
                let rows: Vec<ProviderSecretsRow> = sqlx::query_as(
                    "SELECT id, client_secret, bind_password, client_secret_enc, bind_password_enc, secrets_key_id
                     FROM identity_providers
                     WHERE (secrets_key_id IS NULL AND (client_secret IS NOT NULL OR bind_password IS NOT NULL))
                        OR secrets_key_id <> $1
                     FOR UPDATE",
                )
                .bind(keyring.active_id().0)
                .fetch_all(&mut *conn)
                .await?;
                'rows: for row in rows {
                    let mut resealed: [Option<Sealed>; 2] = [None, None];
                    for (slot, column) in resealed.iter_mut().zip(ProviderSecret::ALL) {
                        let Some(stored) = row.stored(column) else { continue };
                        let secret = match open_provider_secret_to_rewrap(keyring, row.id, column, &stored) {
                            Ok(secret) => secret,
                            Err(OpenError::UnknownKey(k)) => {
                                return Err(PrepareError::Refused(format!(
                                    "identity_providers row {} is encrypted with key {k}, which is not configured",
                                    row.id
                                )));
                            }
                            Err(OpenError::Invalid) => {
                                // Left as it is: sign-in through the provider fails closed until an
                                // administrator enters the secret again.
                                tracing::error!(
                                    provider_id = %row.id,
                                    key = ?row.secrets_key_id.map(KeyId),
                                    "the {} of this identity provider does not decrypt (altered or copied from \
                                     another row); it cannot be re-encrypted. Enter it again under Administration \
                                     > Sign-in",
                                    column.label()
                                );
                                done.failed += 1;
                                continue 'rows;
                            }
                        };
                        *slot = Some(seal_provider_secret(keyring, row.id, column, &secret));
                    }
                    let [client_secret, bind_password] = resealed;
                    // Not an audited change: the configuration stays the same (as for TOTP).
                    sqlx::query(
                        "UPDATE identity_providers SET client_secret_enc = $1, bind_password_enc = $2,
                           secrets_key_id = $3, client_secret = NULL, bind_password = NULL
                         WHERE id = $4",
                    )
                    .bind(client_secret.as_ref().map(|s| &s.bytes))
                    .bind(bind_password.as_ref().map(|s| &s.bytes))
                    .bind(keyring.active_id().0)
                    .bind(row.id)
                    .execute(&mut *conn)
                    .await?;
                    if row.secrets_key_id.is_none() { done.unencrypted += 1 } else { done.from_previous += 1 }
                }
            }
        }
        Ok(done)
    }
}

// ---------------------------------------------------------------------------
// TOTP seeds
// ---------------------------------------------------------------------------

/// Binds a seed to its user: copied onto another user's row, it does not open.
fn totp_ad(user_id: Uuid) -> Vec<u8> {
    let mut ad = b"shadoucmdb:user_totp:v1:".to_vec();
    ad.extend_from_slice(user_id.as_bytes());
    ad
}

pub fn seal_totp_secret(keyring: &Keyring, user_id: Uuid, seed: &[u8]) -> Sealed {
    keyring.seal(Purpose::TotpSecret, &totp_ad(user_id), seed)
}

/// The seed in a `user_totp` row, for sign-in. A seed with `key_id` NULL is
/// refused (GH#416): start-up encrypts every such row before the server
/// listens, so one seen here was written to the database directly, or by an
/// older release running next to this one, and is not trusted.
pub fn open_totp_secret(
    keyring: &Keyring,
    user_id: Uuid,
    key_id: Option<i32>,
    stored: &[u8],
) -> Result<Secret, OpenError> {
    match key_id {
        None => {
            tracing::warn!(
                user_id = %user_id,
                "the authenticator secret of this user is stored unencrypted and is refused; a restart encrypts \
                 it. Find out how it was written: this release encrypts every secret it stores"
            );
            Err(OpenError::Invalid)
        }
        Some(k) => keyring.open(Purpose::TotpSecret, KeyId(k), &totp_ad(user_id), stored),
    }
}

/// [`open_totp_secret`], also reading a seed stored before encryption (0025):
/// only for the start-up step that encrypts it.
fn open_totp_secret_to_rewrap(
    keyring: &Keyring,
    user_id: Uuid,
    key_id: Option<i32>,
    stored: &[u8],
) -> Result<Secret, OpenError> {
    match key_id {
        None if stored.len() == 20 => Ok(Secret::new(stored.to_vec())),
        None => Err(OpenError::Invalid),
        Some(_) => open_totp_secret(keyring, user_id, key_id, stored),
    }
}

// ---------------------------------------------------------------------------
// Identity provider secrets
// ---------------------------------------------------------------------------

/// A secret column of `identity_providers`. Only one applies to a kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSecret {
    /// OIDC `client_secret_enc`
    ClientSecret,
    /// LDAP `bind_password_enc`
    BindPassword,
}

impl ProviderSecret {
    pub const ALL: [ProviderSecret; 2] = [ProviderSecret::ClientSecret, ProviderSecret::BindPassword];

    /// The column name in the associated data (the plaintext column's name).
    fn column(self) -> &'static str {
        match self {
            ProviderSecret::ClientSecret => "client_secret",
            ProviderSecret::BindPassword => "bind_password",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ProviderSecret::ClientSecret => "client secret",
            ProviderSecret::BindPassword => "bind password",
        }
    }
}

/// A provider secret as stored. `Debug` never shows it; it is decrypted only
/// where it is presented to the provider ([`open_provider_secret`]).
#[derive(Clone, PartialEq, Eq)]
pub enum StoredSecret {
    /// Written before encryption (0026); start-up encrypts it. The previous
    /// release cannot use encrypted secrets, so it must not run next to this
    /// one.
    Plain(String),
    Encrypted {
        key_id: KeyId,
        bytes: Vec<u8>,
    },
}

impl std::fmt::Debug for StoredSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sealed(<redacted>)")
    }
}

impl StoredSecret {
    /// From the plaintext column, the `_enc` column and the row's key id. The
    /// check constraints never let a row hold both forms.
    pub fn from_columns(plain: Option<String>, sealed: Option<Vec<u8>>, key_id: Option<i32>) -> Option<StoredSecret> {
        match (sealed, key_id) {
            (Some(bytes), Some(k)) => Some(StoredSecret::Encrypted { key_id: KeyId(k), bytes }),
            _ => plain.map(StoredSecret::Plain),
        }
    }
}

/// Binds a secret to its provider and column: copied onto another provider, or
/// from the bind password to the client secret, it does not open.
fn provider_ad(provider_id: Uuid, column: ProviderSecret) -> Vec<u8> {
    let mut ad = b"shadoucmdb:identity_providers:v1:".to_vec();
    ad.extend_from_slice(provider_id.as_bytes());
    ad.push(b':');
    ad.extend_from_slice(column.column().as_bytes());
    ad
}

pub fn seal_provider_secret(keyring: &Keyring, provider_id: Uuid, column: ProviderSecret, secret: &str) -> Sealed {
    keyring.seal(Purpose::IdentityProviderSecret, &provider_ad(provider_id, column), secret.as_bytes())
}

/// The secret in clear, to present to the provider. An unencrypted one is
/// refused, as in [`open_totp_secret`].
pub fn open_provider_secret(
    keyring: &Keyring,
    provider_id: Uuid,
    column: ProviderSecret,
    stored: &StoredSecret,
) -> Result<crate::auth::secret::Secret, OpenError> {
    match stored {
        StoredSecret::Plain(_) => {
            tracing::warn!(
                provider_id = %provider_id,
                "the {} of this identity provider is stored unencrypted and is refused; a restart encrypts it. \
                 Find out how it was written: this release encrypts every secret it stores",
                column.label()
            );
            Err(OpenError::Invalid)
        }
        StoredSecret::Encrypted { key_id, bytes } => {
            let plain =
                keyring.open(Purpose::IdentityProviderSecret, *key_id, &provider_ad(provider_id, column), bytes)?;
            let text = std::str::from_utf8(&plain).map_err(|_| OpenError::Invalid)?;
            Ok(text.into())
        }
    }
}

/// [`open_provider_secret`], also reading a secret stored before encryption
/// (0026): only for the start-up step that encrypts it.
fn open_provider_secret_to_rewrap(
    keyring: &Keyring,
    provider_id: Uuid,
    column: ProviderSecret,
    stored: &StoredSecret,
) -> Result<crate::auth::secret::Secret, OpenError> {
    match stored {
        StoredSecret::Plain(s) => Ok(s.as_str().into()),
        StoredSecret::Encrypted { .. } => open_provider_secret(keyring, provider_id, column, stored),
    }
}

/// The secret columns of one provider, as the start-up step reads them.
#[derive(sqlx::FromRow)]
struct ProviderSecretsRow {
    id: Uuid,
    client_secret: Option<String>,
    bind_password: Option<String>,
    client_secret_enc: Option<Vec<u8>>,
    bind_password_enc: Option<Vec<u8>>,
    secrets_key_id: Option<i32>,
}

impl ProviderSecretsRow {
    fn stored(&self, column: ProviderSecret) -> Option<StoredSecret> {
        let (plain, sealed) = match column {
            ProviderSecret::ClientSecret => (&self.client_secret, &self.client_secret_enc),
            ProviderSecret::BindPassword => (&self.bind_password, &self.bind_password_enc),
        };
        StoredSecret::from_columns(plain.clone(), sealed.clone(), self.secrets_key_id)
    }
}

// ---------------------------------------------------------------------------
// Counts, refusal and warnings
// ---------------------------------------------------------------------------

/// Rows of one table encrypted under one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyCount {
    pub table: SealedTable,
    pub key_id: KeyId,
    pub rows: i64,
}

/// Whether the table has its key column: not before its migration, e.g. when
/// `backup` or `verify` run against a database at an older level.
async fn has_key_column(conn: &mut PgConnection, table: SealedTable) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_attribute
                        WHERE attrelid = to_regclass('cmdb.' || $1) AND attname = $2 AND NOT attisdropped)",
    )
    .bind(table.name())
    .bind(table.key_column())
    .fetch_one(conn)
    .await
}

/// Encrypted rows per table and key id.
pub async fn key_counts(conn: &mut PgConnection) -> sqlx::Result<Vec<KeyCount>> {
    let mut out = Vec::new();
    for &table in SEALED_TABLES {
        if !has_key_column(conn, table).await? {
            continue;
        }
        let col = table.key_column();
        let rows: Vec<(i32, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {col}, count(*) FROM cmdb.{} WHERE {col} IS NOT NULL GROUP BY 1 ORDER BY 1",
            table.name()
        )))
        .fetch_all(&mut *conn)
        .await?;
        out.extend(rows.into_iter().map(|(k, rows)| KeyCount { table, key_id: KeyId(k), rows }));
    }
    Ok(out)
}

/// Rows per table not encrypted yet (written before encryption).
pub async fn unencrypted_counts(conn: &mut PgConnection) -> sqlx::Result<Vec<(SealedTable, i64)>> {
    let mut out = Vec::new();
    for &table in SEALED_TABLES {
        let migrated = has_key_column(conn, table).await?;
        // Before the migration every secret is plaintext, if the table exists at all.
        if !migrated {
            let exists: bool = sqlx::query_scalar("SELECT to_regclass('cmdb.' || $1) IS NOT NULL")
                .bind(table.name())
                .fetch_one(&mut *conn)
                .await?;
            if !exists {
                continue;
            }
        }
        let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM cmdb.{} WHERE {}",
            table.name(),
            table.unencrypted_filter(migrated)
        )))
        .fetch_one(&mut *conn)
        .await?;
        out.push((table, n));
    }
    Ok(out)
}

/// Per key other than `known`: the key, what it encrypts ("42 authenticator
/// secrets and ...") and whether that is one row; plus the tables involved.
type Foreign = (Vec<(KeyId, String, bool)>, Vec<SealedTable>);

fn foreign(counts: &[KeyCount], known: &[KeyId]) -> Option<Foreign> {
    let mut by_key: BTreeMap<KeyId, Vec<&KeyCount>> = BTreeMap::new();
    for c in counts.iter().filter(|c| c.rows > 0 && !known.contains(&c.key_id)) {
        by_key.entry(c.key_id).or_default().push(c);
    }
    if by_key.is_empty() {
        return None;
    }
    let mut tables: Vec<SealedTable> = by_key.values().flatten().map(|c| c.table).collect();
    tables.sort();
    tables.dedup();
    let keys = by_key
        .iter()
        .map(|(key, cs)| {
            let parts: Vec<String> = cs.iter().map(|c| c.table.describe(c.rows)).collect();
            (*key, parts.join(" and "), cs.len() == 1 && cs[0].rows == 1)
        })
        .collect();
    Some((keys, tables))
}

/// `run "a" (does x) and "b" (does y)`.
fn reset_advice(tables: &[SealedTable]) -> String {
    let cmds: Vec<String> = tables
        .iter()
        .map(|t| {
            let (cmd, effect) = t.reset();
            format!("\"{cmd}\" ({effect})")
        })
        .collect();
    cmds.join(" and ")
}

/// Why `serve` must not start: rows under a key that is neither the active
/// nor the previous one. `None` when every row can be read.
pub fn refusal(counts: &[KeyCount], active: KeyId, previous: Option<KeyId>) -> Option<String> {
    let known: Vec<KeyId> = [Some(active), previous].into_iter().flatten().collect();
    let (keys, tables) = foreign(counts, &known)?;
    let what: Vec<String> = keys
        .iter()
        .map(|(key, what, one)| format!("{what} {} encrypted with key {key}", if *one { "is" } else { "are" }))
        .collect();
    let what = what.join("; ");
    let previous = match previous {
        Some(p) => format!("ENCRYPTION_KEY_PREVIOUS_FILE holds key {p}"),
        None => "ENCRYPTION_KEY_PREVIOUS_FILE is not set".to_owned(),
    };
    Some(format!(
        "{what}, but ENCRYPTION_KEY_FILE holds key {active} (and {previous}). Configure the key this database was \
         encrypted with. If that key is lost, run {}.",
        reset_advice(&tables)
    ))
}

/// For `verify` and `migrate` (GH#416): secrets stored unencrypted. Sign-in
/// refuses them; `serve` encrypts them at start-up. `None` when there are none.
pub fn unencrypted_warning(unencrypted: &[(SealedTable, i64)]) -> Option<String> {
    let parts: Vec<String> = unencrypted.iter().filter(|(_, n)| *n > 0).map(|&(t, n)| t.describe(n)).collect();
    if parts.is_empty() {
        return None;
    }
    let one = unencrypted.iter().map(|(_, n)| n).sum::<i64>() == 1;
    Some(format!(
        "{} {} stored unencrypted. Sign-in refuses {} until `serve` encrypts {} at start-up. This is expected only \
         after an upgrade from a release that stored secrets unencrypted; otherwise find out how {} written, \
         because this release encrypts every secret it stores.",
        parts.join(" and "),
        if one { "is" } else { "are" },
        if one { "it" } else { "them" },
        if one { "it" } else { "them" },
        if one { "it was" } else { "they were" },
    ))
}

/// For `restore`: the backup's rows under keys the configuration does not
/// have. `configured`: the active and previous key ids, `None` without a key.
pub fn restore_warning(counts: &[KeyCount], configured: Option<(KeyId, Option<KeyId>)>) -> Option<String> {
    let known: Vec<KeyId> = configured.map(|(a, p)| [Some(a), p].into_iter().flatten().collect()).unwrap_or_default();
    let (keys, tables) = foreign(counts, &known)?;
    let what: Vec<String> = keys.iter().map(|(key, what, _)| format!("{what} encrypted with key {key}")).collect();
    let what = what.join(" and ");
    let configured = match configured {
        Some((a, Some(p))) => format!("The configured key is {a} (previous key {p})"),
        Some((a, None)) => format!("The configured key is {a}"),
        None => "No key is configured (ENCRYPTION_KEY_FILE is not set)".to_owned(),
    };
    let cmds: Vec<String> = tables.iter().map(|t| format!("\"{}\"", t.reset().0)).collect();
    Some(format!(
        "This backup holds {what}. {configured}. The server will not start until that key is configured, or until \
         {} {} run; the key is never part of a backup.",
        cmds.join(" and "),
        if cmds.len() == 1 { "has been" } else { "have been" }
    ))
}

// ---------------------------------------------------------------------------
// Start-up
// ---------------------------------------------------------------------------

/// What [`prepare`] did to one table.
#[derive(Debug, Default)]
pub struct Rewrapped {
    pub table: SealedTable,
    /// Rows written before encryption, now encrypted.
    pub unencrypted: u64,
    /// Rows under the previous key, now under the active one.
    pub from_previous: u64,
    /// Rows under a configured key that did not decrypt; left as they are.
    pub failed: u64,
}

impl Default for SealedTable {
    fn default() -> Self {
        SEALED_TABLES[0]
    }
}

#[derive(Debug)]
pub enum PrepareError {
    /// Rows under a key that is not configured; the message says what to do.
    Refused(String),
    Database(sqlx::Error),
}

impl From<sqlx::Error> for PrepareError {
    fn from(e: sqlx::Error) -> Self {
        PrepareError::Database(e)
    }
}

/// The self-check and re-encryption `serve` runs before it accepts requests.
/// All rows or none: a crash leaves the old state and the next start retries.
pub async fn prepare(pool: &PgPool, keyring: &Keyring) -> Result<Vec<Rewrapped>, PrepareError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('shadoucmdb:sealed-secrets'))").execute(&mut *tx).await?;
    let counts = key_counts(&mut tx).await?;
    if let Some(message) = refusal(&counts, keyring.active_id(), keyring.previous_id()) {
        return Err(PrepareError::Refused(message));
    }
    let mut done = Vec::new();
    for &table in SEALED_TABLES {
        done.push(table.rewrap(&mut tx, keyring).await?);
    }
    tx.commit().await?;
    for d in &done {
        if d.unencrypted + d.from_previous > 0 {
            let previous = keyring.previous_id().map(|p| format!(" {p}")).unwrap_or_default();
            tracing::info!(
                table = d.table.name(),
                "Encrypted {} (key {}): {} unencrypted, {} from previous key{previous}",
                d.table.describe((d.unencrypted + d.from_previous) as i64),
                keyring.active_id(),
                d.unencrypted,
                d.from_previous,
            );
        }
    }
    Ok(done)
}

// ---------------------------------------------------------------------------
// Lost key
// ---------------------------------------------------------------------------

/// A user whose authenticator secret is under a key that is not configured.
#[derive(Debug)]
pub struct Undecryptable {
    pub user_id: Uuid,
    pub username: String,
    pub confirmed: bool,
    pub key_id: KeyId,
}

/// The `user_totp` rows under keys other than `known`, locked.
pub async fn undecryptable_totp(conn: &mut PgConnection, known: &[KeyId]) -> sqlx::Result<Vec<Undecryptable>> {
    let known: Vec<i32> = known.iter().map(|k| k.0).collect();
    let rows: Vec<(Uuid, String, bool, i32)> = sqlx::query_as(
        "SELECT t.user_id, u.username, t.confirmed_at IS NOT NULL, t.key_id
         FROM user_totp t JOIN users u ON u.id = t.user_id
         WHERE t.key_id IS NOT NULL AND t.key_id <> ALL ($1)
         ORDER BY u.username
         FOR UPDATE OF t",
    )
    .bind(&known)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(user_id, username, confirmed, k)| Undecryptable { user_id, username, confirmed, key_id: KeyId(k) })
        .collect())
}

/// A provider whose secret is under a key that is not configured.
#[derive(Debug)]
pub struct UndecryptableProvider {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub is_enabled: bool,
    pub key_id: KeyId,
}

/// The `identity_providers` rows under keys other than `known`, locked.
pub async fn undecryptable_providers(
    conn: &mut PgConnection,
    known: &[KeyId],
) -> sqlx::Result<Vec<UndecryptableProvider>> {
    let known: Vec<i32> = known.iter().map(|k| k.0).collect();
    let rows: Vec<(Uuid, String, String, bool, i32)> = sqlx::query_as(
        "SELECT id, name, kind, is_enabled, secrets_key_id FROM identity_providers
         WHERE secrets_key_id IS NOT NULL AND secrets_key_id <> ALL ($1)
         ORDER BY lower(name), id
         FOR UPDATE",
    )
    .bind(&known)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, name, kind, is_enabled, k)| UndecryptableProvider { id, name, kind, is_enabled, key_id: KeyId(k) })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(key: i32, rows: i64) -> KeyCount {
        KeyCount { table: SealedTable::UserTotp, key_id: KeyId(key), rows }
    }

    #[test]
    fn refusal_names_both_keys_and_the_reset_command() {
        let (a, b) = (KeyId(0x3f9a01c2), KeyId(0x8be4d177u32 as i32));
        assert_eq!(refusal(&[count(b.0, 5)], b, None), None);
        assert_eq!(refusal(&[count(a.0, 5), count(b.0, 1)], b, Some(a)), None);
        let msg = refusal(&[count(a.0, 42), count(b.0, 3)], b, None).unwrap();
        assert_eq!(
            msg,
            "42 authenticator secrets are encrypted with key 3f9a01c2, but ENCRYPTION_KEY_FILE holds key 8be4d177 \
             (and ENCRYPTION_KEY_PREVIOUS_FILE is not set). Configure the key this database was encrypted with. If \
             that key is lost, run \"shadoucmdb mfa reset-undecryptable\" (turns off two-factor sign-in for those \
             users so they can enrol again)."
        );
        let msg = refusal(&[count(1, 1)], b, Some(a)).unwrap();
        assert!(msg.starts_with("1 authenticator secret is encrypted with key 00000001"), "{msg}");
        assert!(msg.contains("ENCRYPTION_KEY_PREVIOUS_FILE holds key 3f9a01c2"), "{msg}");
    }

    #[test]
    fn refusal_lists_both_tables_and_both_commands() {
        let (a, b) = (KeyId(0x3f9a01c2), KeyId(0x8be4d177u32 as i32));
        let idp = |key: KeyId, rows| KeyCount { table: SealedTable::IdentityProviders, key_id: key, rows };
        let msg = refusal(&[count(a.0, 42), idp(a, 2)], b, None).unwrap();
        assert_eq!(
            msg,
            "42 authenticator secrets and 2 identity provider secrets are encrypted with key 3f9a01c2, but \
             ENCRYPTION_KEY_FILE holds key 8be4d177 (and ENCRYPTION_KEY_PREVIOUS_FILE is not set). Configure the key \
             this database was encrypted with. If that key is lost, run \"shadoucmdb mfa reset-undecryptable\" \
             (turns off two-factor sign-in for those users so they can enrol again) and \"shadoucmdb \
             identity-providers reset-undecryptable\" (disables those providers and clears their secrets until an \
             administrator enters them again)."
        );
        // Only the command that applies is named.
        let msg = refusal(&[count(b.0, 42), idp(a, 1)], b, None).unwrap();
        assert!(msg.starts_with("1 identity provider secret is encrypted with key 3f9a01c2"), "{msg}");
        assert!(!msg.contains("mfa reset-undecryptable"), "{msg}");
        assert_eq!(refusal(&[count(a.0, 42), idp(b, 2)], b, Some(a)), None);
    }

    #[test]
    fn stored_secrets_never_show_in_debug() {
        let plain = StoredSecret::Plain("hunter2".into());
        let sealed = StoredSecret::Encrypted { key_id: KeyId(1), bytes: b"hunter2-bytes".to_vec() };
        for s in [&plain, &sealed] {
            assert_eq!(format!("{s:?}"), "Sealed(<redacted>)");
        }
        assert_eq!(format!("{:?}", Some(plain)), "Some(Sealed(<redacted>))");
    }

    #[test]
    fn provider_secrets_are_bound_to_provider_and_column() {
        let ring = Keyring::random();
        let (p, q) = (Uuid::new_v4(), Uuid::new_v4());
        let secret = "client-secret-value";
        let s = seal_provider_secret(&ring, p, ProviderSecret::ClientSecret, secret);
        assert_eq!(s.bytes.len(), secret.len() + super::super::OVERHEAD);
        assert!(!s.bytes.windows(secret.len()).any(|w| w == secret.as_bytes()));
        let stored = StoredSecret::Encrypted { key_id: s.key_id, bytes: s.bytes.clone() };
        let open = |id, column| open_provider_secret(&ring, id, column, &stored).map(|s| s.expose().to_owned());
        assert_eq!(open(p, ProviderSecret::ClientSecret).as_deref(), Ok(secret));
        assert_eq!(open(q, ProviderSecret::ClientSecret), Err(OpenError::Invalid), "another provider");
        assert_eq!(open(p, ProviderSecret::BindPassword), Err(OpenError::Invalid), "the other column");
        // The message names the part flipped, never an index derived from the
        // ciphertext (CodeQL treats that as logging the secret).
        for (part, i) in [("nonce", 0), ("ciphertext", 12), ("tag", s.bytes.len() - 1)] {
            let mut bytes = s.bytes.clone();
            bytes[i] ^= 1;
            let tampered = StoredSecret::Encrypted { key_id: s.key_id, bytes };
            let refused = open_provider_secret(&ring, p, ProviderSecret::ClientSecret, &tampered).is_err();
            assert!(refused, "flipped {part} byte");
        }
        // The same AD under the TOTP subkey: another purpose, another key.
        let totp = ring.seal(Purpose::TotpSecret, &provider_ad(p, ProviderSecret::ClientSecret), secret.as_bytes());
        let as_idp = StoredSecret::Encrypted { key_id: totp.key_id, bytes: totp.bytes };
        assert!(open_provider_secret(&ring, p, ProviderSecret::ClientSecret, &as_idp).is_err());
        // The AD layout of the design (SHAA-490 §A2).
        let mut ad = b"shadoucmdb:identity_providers:v1:".to_vec();
        ad.extend_from_slice(p.as_bytes());
        ad.extend_from_slice(b":bind_password");
        assert_eq!(provider_ad(p, ProviderSecret::BindPassword), ad);
    }

    #[test]
    fn unencrypted_warning_counts_both_tables() {
        use SealedTable::{IdentityProviders, UserTotp};
        assert_eq!(unencrypted_warning(&[(UserTotp, 0), (IdentityProviders, 0)]), None);
        assert_eq!(unencrypted_warning(&[]), None);
        let msg = unencrypted_warning(&[(UserTotp, 1), (IdentityProviders, 0)]).unwrap();
        assert!(msg.starts_with("1 authenticator secret is stored unencrypted. Sign-in refuses it until"), "{msg}");
        let msg = unencrypted_warning(&[(UserTotp, 3), (IdentityProviders, 1)]).unwrap();
        assert!(
            msg.starts_with(
                "3 authenticator secrets and 1 identity provider secret are stored unencrypted. Sign-in refuses them"
            ),
            "{msg}"
        );
    }

    /// GH#416: only the start-up step that encrypts them reads unencrypted secrets.
    #[test]
    fn unencrypted_secrets_open_only_to_be_encrypted() {
        let ring = Keyring::random();
        let (user, provider) = (Uuid::new_v4(), Uuid::new_v4());
        let seed = [7u8; 20];
        assert_eq!(open_totp_secret(&ring, user, None, &seed).err(), Some(OpenError::Invalid));
        assert_eq!(open_totp_secret_to_rewrap(&ring, user, None, &seed).unwrap().to_vec(), seed);
        assert_eq!(open_totp_secret_to_rewrap(&ring, user, None, &seed[..19]).err(), Some(OpenError::Invalid));
        let sealed = seal_totp_secret(&ring, user, &seed);
        for open in [open_totp_secret, open_totp_secret_to_rewrap] {
            assert_eq!(open(&ring, user, Some(sealed.key_id.0), &sealed.bytes).unwrap().to_vec(), seed);
        }

        let plain = StoredSecret::Plain("hunter2".into());
        let column = ProviderSecret::BindPassword;
        assert_eq!(open_provider_secret(&ring, provider, column, &plain).err(), Some(OpenError::Invalid));
        let opened = open_provider_secret_to_rewrap(&ring, provider, column, &plain).unwrap();
        assert_eq!(opened.expose(), "hunter2");
        let s = seal_provider_secret(&ring, provider, column, "hunter2");
        let sealed = StoredSecret::Encrypted { key_id: s.key_id, bytes: s.bytes };
        for open in [open_provider_secret, open_provider_secret_to_rewrap] {
            assert_eq!(open(&ring, provider, column, &sealed).unwrap().expose(), "hunter2");
        }
    }

    #[test]
    fn restore_warning_per_table() {
        let a = KeyId(0x3f9a01c2);
        assert_eq!(restore_warning(&[count(a.0, 2)], Some((a, None))), None);
        let msg = restore_warning(&[count(a.0, 42)], None).unwrap();
        assert_eq!(
            msg,
            "This backup holds 42 authenticator secrets encrypted with key 3f9a01c2. No key is configured \
             (ENCRYPTION_KEY_FILE is not set). The server will not start until that key is configured, or until \
             \"shadoucmdb mfa reset-undecryptable\" has been run; the key is never part of a backup."
        );
        let msg = restore_warning(&[count(a.0, 1)], Some((KeyId(7), Some(KeyId(8))))).unwrap();
        assert!(msg.contains("The configured key is 00000007 (previous key 00000008)."), "{msg}");
    }
}
