//! The commands around the encryption key: `generate-encryption-key`,
//! `mfa reset-undecryptable`, `identity-providers reset-undecryptable` and
//! `webhooks reset-undecryptable`
//! (the audited ways out when a key is lost), and the key report `verify` and
//! `migrate` print.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use serde_json::json;
use sqlx::{PgConnection, PgPool};

use super::sealed::{self, KeyCount};
use super::{KeyId, configured_key_ids, encode_key, key_id, new_key};
use crate::api::context::RequestContext;
use crate::auth::events;
use crate::config::{Config, EncryptionConfig};
use crate::data::crud::AuditAction;
use crate::data::mfa as mfa_data;
use crate::{db, maintenance};

#[derive(Debug, Args)]
pub struct GenerateKeyArgs {
    /// Write the key to this file. It must not exist yet: a key is never overwritten.
    #[arg(long, value_name = "PATH")]
    pub out: PathBuf,
}

/// Writes a new random key to a new file (mode 0600 on Unix). Offline.
pub fn generate_key(args: GenerateKeyArgs) -> anyhow::Result<()> {
    let key = new_key();
    // The env file needs the full path: a service does not start in this directory.
    let path = &std::path::absolute(&args.out).unwrap_or_else(|_| args.out.clone());
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            anyhow::anyhow!("{} already exists; a key file is never overwritten, choose another path", path.display())
        } else {
            anyhow::Error::new(e).context(format!("cannot create {}", path.display()))
        }
    })?;
    let mut content = encode_key(&key).into_bytes();
    let written = file.write_all(&content).and_then(|()| file.sync_all());
    super::wipe(&mut content);
    written.with_context(|| format!("cannot write {}", path.display()))?;
    println!("Wrote a new encryption key to {} (key {}).", path.display(), key_id(&key));
    println!();
    println!("Next steps:");
    println!("  1. Set ENCRYPTION_KEY_FILE={} in the env file of the service.", path.display());
    #[cfg(unix)]
    println!("  2. Let only the service account read it, e.g. chown root:shadoucmdb and chmod 640.");
    #[cfg(not(unix))]
    println!(
        "  2. Restrict the file's ACL, e.g. icacls \"{}\" /inheritance:r /grant:r \"Administrators:F\" \"SYSTEM:F\" \
         \"NT SERVICE\\ShadouCMDB:R\"",
        path.display()
    );
    println!("  3. Keep a copy apart from the database backups (password vault or escrow): without it, restored");
    println!("     backups have no usable two-factor enrolments or identity provider secrets.");
    Ok(())
}

#[derive(Debug, Subcommand)]
pub enum MfaCommand {
    /// Turn off two-factor sign-in for users whose authenticator secret is
    /// encrypted with a key that is not configured (the key is lost), so they
    /// can set it up again. Audited; asks for confirmation. Refuses to run
    /// without ENCRYPTION_KEY_FILE unless --no-key is given.
    ResetUndecryptable(ResetUndecryptableArgs),
}

#[derive(Debug, Args)]
pub struct ResetUndecryptableArgs {
    /// List the users concerned and change nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// The key is lost and no ENCRYPTION_KEY_FILE is set: treat every
    /// encrypted secret as undecryptable. Without it the command refuses to
    /// run when ENCRYPTION_KEY_FILE is not set, so that a shell missing the
    /// service's environment does not reset everyone.
    #[arg(long)]
    pub no_key: bool,
    #[command(flatten)]
    pub confirm: maintenance::ConfirmArgs,
}

/// The configured key ids, the ones whose secrets are kept. Without a key,
/// only `--no-key` lets every encrypted secret count as undecryptable
/// (GH#223): `--yes` alone is not enough.
fn known_keys(configured: Option<(KeyId, Option<KeyId>)>, no_key: bool, secrets: &str) -> anyhow::Result<Vec<KeyId>> {
    match configured {
        None if !no_key => bail!(
            "ENCRYPTION_KEY_FILE is not set, so every encrypted {secrets} would count as undecryptable. Nothing was \
             changed. Run the command with the service's environment (e.g. --env-file <the service's env file>); \
             only if the key is lost and none is configured, pass --no-key."
        ),
        Some(_) if no_key => bail!(
            "--no-key was given, but ENCRYPTION_KEY_FILE is set. Nothing was changed. Leave out --no-key to keep the \
             secrets under the configured key, or unset ENCRYPTION_KEY_FILE if the key is lost."
        ),
        None => Ok(Vec::new()),
        Some((a, p)) => Ok([Some(a), p].into_iter().flatten().collect()),
    }
}

pub async fn mfa(cfg: &Config, cmd: MfaCommand) -> anyhow::Result<()> {
    match cmd {
        MfaCommand::ResetUndecryptable(args) => reset_undecryptable(cfg, args).await,
    }
}

async fn reset_undecryptable(cfg: &Config, args: ResetUndecryptableArgs) -> anyhow::Result<()> {
    // Only the ids are needed: the rows are deleted, not decrypted.
    let configured = configured_key_ids(&cfg.encryption)?;
    known_keys(configured, args.no_key, "authenticator secret")?;
    let pool = db::connect(&cfg.database).await?;
    let result = reset_mfa(&pool, configured, &args).await;
    pool.close().await;
    result
}

pub(crate) async fn reset_mfa(
    pool: &PgPool,
    configured: Option<(KeyId, Option<KeyId>)>,
    args: &ResetUndecryptableArgs,
) -> anyhow::Result<()> {
    let known = known_keys(configured, args.no_key, "authenticator secret")?;
    if db::applied_count(pool).await? != db::expected_count() {
        bail!("the database is not fully migrated; run `shadoucmdb migrate` first");
    }
    let mut tx = pool.begin().await?;
    let users = sealed::undecryptable_totp(&mut tx, &known).await?;
    match configured {
        None => {
            println!("No key is configured (--no-key): every encrypted authenticator secret counts as undecryptable.")
        }
        Some((a, None)) => println!("Configured key: {a}."),
        Some((a, Some(p))) => println!("Configured keys: {a} (previous key {p})."),
    }
    if users.is_empty() {
        println!("No authenticator secret is encrypted with another key; nothing to do.");
        return Ok(());
    }
    println!("Authenticator secrets encrypted with a key that is not configured ({}):", users.len());
    for u in &users {
        let state = if u.confirmed { "two-factor sign-in on" } else { "set-up not finished" };
        println!("  {:<32} key {}  {state}", u.username, u.key_id);
    }
    if args.dry_run {
        println!("Dry run: nothing was changed.");
        return Ok(());
    }
    let (database, place) = maintenance::describe(&mut tx).await?;
    let action = format!("turn off two-factor sign-in for {} of", users_n(users.len()));
    maintenance::confirm(&action, &database, &place, args.confirm.yes)?;
    let actor = match crate::prune::operator() {
        Some(op) => format!("cli: mfa reset-undecryptable ({op})"),
        None => "cli: mfa reset-undecryptable".to_owned(),
    };
    reset_users(&mut tx, &RequestContext::system(actor, "cli"), &users).await?;
    tx.commit().await?;
    println!(
        "Turned off two-factor sign-in for {}. They set it up again at their next sign-in (at once where a \
         profile requires it).",
        users_n(users.len())
    );
    Ok(())
}

fn users_n(n: usize) -> String {
    format!("{n} {}", if n == 1 { "user" } else { "users" })
}

/// Deletes the users' authenticator, recovery codes and pending sign-ins, with
/// one `mfa.disable` event each (`reason: key_lost`).
pub(crate) async fn reset_users(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    users: &[sealed::Undecryptable],
) -> sqlx::Result<()> {
    for u in users {
        mfa_data::delete_mfa(conn, u.user_id).await?;
        let extra = json!({ "reason": "key_lost", "keyId": u.key_id.to_string() });
        events::mfa(conn, ctx, AuditAction::MfaDisable, u.user_id, &u.username, extra).await?;
    }
    Ok(())
}

#[derive(Debug, Subcommand)]
pub enum IdentityProvidersCommand {
    /// Disable the identity providers whose secret (OIDC client secret, LDAP
    /// bind password) is encrypted with a key that is not configured (the key
    /// is lost) and clear that secret, so the server can start. Sign-in
    /// through them stops until an administrator enters the secret again and
    /// enables them. Audited; asks for confirmation. Refuses to run without
    /// ENCRYPTION_KEY_FILE unless --no-key is given.
    ResetUndecryptable(ResetUndecryptableArgs),
}

pub async fn identity_providers(cfg: &Config, cmd: IdentityProvidersCommand) -> anyhow::Result<()> {
    match cmd {
        IdentityProvidersCommand::ResetUndecryptable(args) => reset_providers(cfg, args).await,
    }
}

async fn reset_providers(cfg: &Config, args: ResetUndecryptableArgs) -> anyhow::Result<()> {
    // Only the ids are needed: the secrets are cleared, not decrypted.
    let configured = configured_key_ids(&cfg.encryption)?;
    known_keys(configured, args.no_key, "identity provider secret")?;
    let pool = db::connect(&cfg.database).await?;
    let result = reset_idps(&pool, cfg.auth.public_url.as_deref(), configured, &args).await;
    pool.close().await;
    result
}

pub(crate) async fn reset_idps(
    pool: &PgPool,
    public_url: Option<&str>,
    configured: Option<(KeyId, Option<KeyId>)>,
    args: &ResetUndecryptableArgs,
) -> anyhow::Result<()> {
    let known = known_keys(configured, args.no_key, "identity provider secret")?;
    if db::applied_count(pool).await? != db::expected_count() {
        bail!("the database is not fully migrated; run `shadoucmdb migrate` first");
    }
    let mut tx = pool.begin().await?;
    let providers = sealed::undecryptable_providers(&mut tx, &known).await?;
    match configured {
        None => println!(
            "No key is configured (--no-key): every encrypted identity provider secret counts as undecryptable."
        ),
        Some((a, None)) => println!("Configured key: {a}."),
        Some((a, Some(p))) => println!("Configured keys: {a} (previous key {p})."),
    }
    if providers.is_empty() {
        println!("No identity provider secret is encrypted with another key; nothing to do.");
        return Ok(());
    }
    println!("Identity providers whose secret is encrypted with a key that is not configured ({}):", providers.len());
    for p in &providers {
        let (kind, field) = if p.kind == "ldap" { ("LDAP", "bind password") } else { ("OIDC", "client secret") };
        let state = if p.is_enabled { "enabled" } else { "disabled" };
        println!("  {:<32} {kind:<4}  key {}  {state}, {field}", p.name, p.key_id);
    }
    if args.dry_run {
        println!("Dry run: nothing was changed.");
        return Ok(());
    }
    println!(
        "Each is disabled and its secret cleared (an LDAP directory also loses its bind DN; the old one stays in \
         the audit trail). Sign-in through them stops, and the sessions of their accounts end, until an \
         administrator enters the secret again under Administration > Sign-in and enables the provider."
    );
    let (database, place) = maintenance::describe(&mut tx).await?;
    let action = format!("disable {} and clear their secrets in", providers_n(providers.len()));
    maintenance::confirm(&action, &database, &place, args.confirm.yes)?;
    let actor = match crate::prune::operator() {
        Some(op) => format!("cli: identity-providers reset-undecryptable ({op})"),
        None => "cli: identity-providers reset-undecryptable".to_owned(),
    };
    let ctx = RequestContext::system(actor, "cli");
    crate::modules::identity_providers::reset_undecryptable(&mut tx, &ctx, public_url, &providers)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e.message))?;
    tx.commit().await?;
    println!(
        "Disabled {} and cleared their secrets. Enter each secret again and enable the provider under \
         Administration > Sign-in.",
        providers_n(providers.len())
    );
    Ok(())
}

fn providers_n(n: usize) -> String {
    format!("{n} identity {}", if n == 1 { "provider" } else { "providers" })
}

#[derive(Debug, Subcommand)]
pub enum WebhooksCommand {
    /// Suspend the webhook endpoints whose signing secret, previous secret or
    /// auth header is encrypted with a key that is not configured (the key is
    /// lost), so the server can start: each gets a new signing secret that is
    /// never shown and loses a header it cannot decrypt. Their deliveries are
    /// held until an administrator rotates the secret, shares it with the
    /// receiver and resumes the endpoint. Audited; asks for confirmation.
    /// Refuses to run without ENCRYPTION_KEY_FILE unless --no-key is given.
    ResetUndecryptable(ResetUndecryptableArgs),
}

pub async fn webhooks(cfg: &Config, cmd: WebhooksCommand) -> anyhow::Result<()> {
    match cmd {
        WebhooksCommand::ResetUndecryptable(args) => {
            // The secrets that do not decrypt are replaced, not read: a new one is sealed
            // under the configured key, so the key itself is needed unless --no-key.
            let configured = configured_key_ids(&cfg.encryption)?;
            known_keys(configured, args.no_key, "webhook endpoint secret")?;
            let keyring = match configured {
                Some(_) => Some(crate::secrets::Keyring::load(&cfg.encryption)?),
                None => None,
            };
            let pool = db::connect(&cfg.database).await?;
            let result = reset_endpoints(&pool, keyring.as_ref(), configured, &args).await;
            pool.close().await;
            result
        }
    }
}

pub(crate) async fn reset_endpoints(
    pool: &PgPool,
    keyring: Option<&crate::secrets::Keyring>,
    configured: Option<(KeyId, Option<KeyId>)>,
    args: &ResetUndecryptableArgs,
) -> anyhow::Result<()> {
    let known = known_keys(configured, args.no_key, "webhook endpoint secret")?;
    if db::applied_count(pool).await? != db::expected_count() {
        bail!("the database is not fully migrated; run `shadoucmdb migrate` first");
    }
    let mut tx = pool.begin().await?;
    let endpoints = sealed::undecryptable_endpoints(&mut tx, &known).await?;
    if endpoints.is_empty() {
        println!("No webhook endpoint secret is encrypted with another key; nothing to do.");
        return Ok(());
    }
    println!("Webhook endpoints with a secret encrypted with a key that is not configured ({}):", endpoints.len());
    for e in &endpoints {
        let keys: Vec<String> = e.key_ids.iter().map(ToString::to_string).collect();
        println!("  {:<32} {:<32} {:<10} key {}", e.key, e.name, e.status, keys.join(", "));
    }
    if args.dry_run {
        println!("Dry run: nothing was changed.");
        return Ok(());
    }
    let Some(keyring) = keyring else {
        bail!(
            "No key is configured (--no-key): the endpoints need a new signing secret sealed under a key. Configure \
             ENCRYPTION_KEY_FILE (a new key: `shadoucmdb generate-encryption-key`) and run the command again. Nothing \
             was changed."
        );
    };
    println!(
        "Each is suspended (secret_required) and gets a new signing secret that is not shown; a header that does not \
         decrypt is removed. Its deliveries are held until an administrator rotates the secret under Administration > \
         Webhooks, shares it with the receiver and resumes the endpoint."
    );
    let (database, place) = maintenance::describe(&mut tx).await?;
    let action = format!("suspend {} and replace their secrets in", endpoints_n(endpoints.len()));
    maintenance::confirm(&action, &database, &place, args.confirm.yes)?;
    let actor = match crate::prune::operator() {
        Some(op) => format!("cli: webhooks reset-undecryptable ({op})"),
        None => "cli: webhooks reset-undecryptable".to_owned(),
    };
    let ctx = RequestContext::system(actor, "cli");
    crate::modules::webhooks::service::reset_undecryptable(&mut tx, &ctx, keyring, &endpoints)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e.message))?;
    tx.commit().await?;
    println!(
        "Suspended {} and replaced their secrets. Rotate each secret, share it with the receiver and resume the \
         endpoint under Administration > Webhooks.",
        endpoints_n(endpoints.len())
    );
    Ok(())
}

fn endpoints_n(n: usize) -> String {
    format!("{n} webhook {}", if n == 1 { "endpoint" } else { "endpoints" })
}

/// The encrypted secrets per table and key, against the configured key: for
/// `verify` and `migrate`, so a missing or wrong key shows before `serve`
/// refuses to start. Warns only: neither command needs the key.
pub async fn report(conn: &mut PgConnection, cfg: &EncryptionConfig) -> anyhow::Result<()> {
    let counts = sealed::key_counts(conn).await?;
    let unencrypted = sealed::unencrypted_counts(conn).await?;
    println!("Encrypted secrets (ENCRYPTION_KEY_FILE):");
    let configured = match configured_key_ids(cfg) {
        Ok(Some((a, p))) => {
            match p {
                Some(p) => println!("  configured key {a}, previous key {p}"),
                None => println!("  configured key {a}"),
            }
            Some((a, p))
        }
        Ok(None) => {
            println!(
                "  WARNING: ENCRYPTION_KEY_FILE is not set. `serve` does not start without it. Create a key with \
                 \"shadoucmdb generate-encryption-key --out <path>\" and set ENCRYPTION_KEY_FILE before restarting \
                 the service."
            );
            None
        }
        Err(e) => {
            println!("  WARNING: {e:#}. `serve` does not start until this is fixed.");
            None
        }
    };
    print_counts(&counts, &unencrypted, configured.map(|(a, _)| a));
    if let Some(message) = configured.and_then(|(a, p)| sealed::refusal(&counts, a, p)) {
        println!("  WARNING: {message}");
    }
    if let Some(message) = sealed::unencrypted_warning(&unencrypted) {
        println!("  WARNING: {message}");
    }
    Ok(())
}

fn print_counts(counts: &[KeyCount], unencrypted: &[(sealed::SealedTable, i64)], active: Option<KeyId>) {
    for &(table, n) in unencrypted {
        let mut line = format!("  {:<18} ", table.name());
        let by_key: Vec<String> = counts
            .iter()
            .filter(|c| c.table == table)
            .map(|c| {
                let note = if Some(c.key_id) == active { " (configured)" } else { "" };
                format!("{} under key {}{note}", c.rows, c.key_id)
            })
            .collect();
        let mut parts = by_key;
        if n > 0 {
            parts.push(format!("{n} not encrypted yet (`serve` encrypts them at start-up)"));
        }
        if parts.is_empty() {
            parts.push("none".to_owned());
        }
        line.push_str(&parts.join(", "));
        println!("{line}");
    }
}
