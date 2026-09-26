//! `shadoucmdb create-admin`: create a user holding the Administrator profile
//! from the command line. Works on a fresh install (instead of the first-run
//! screen) and later as the way back in when every administrator is locked out.

use std::io::{BufRead, IsTerminal};

use anyhow::{Context, bail};
use clap::Args;

use crate::api::context::RequestContext;
use crate::config::DatabaseConfig;
use crate::data::auth as data;
use crate::db;
use crate::modules::users::{self, UserCreate};

#[derive(Debug, Args)]
pub struct CreateAdminArgs {
    /// Sign-in name (letters, digits, ".", "_", "@", "-"; unique regardless of case).
    #[arg(long)]
    pub username: String,
    /// Name shown in the UI (defaults to the username).
    #[arg(long)]
    pub display_name: Option<String>,
    #[arg(long)]
    pub email: Option<String>,
    /// Read the password from the first line of stdin instead of prompting
    /// (for scripts: `printf '%s\n' "$PW" | shadoucmdb create-admin --username admin --password-stdin`).
    #[arg(long)]
    pub password_stdin: bool,
}

fn read_password(from_stdin: bool) -> anyhow::Result<String> {
    if from_stdin {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).context("cannot read the password from stdin")?;
        return Ok(line.trim_end_matches(['\r', '\n']).to_owned());
    }
    if !std::io::stdin().is_terminal() {
        bail!("no terminal to prompt for the password; pass --password-stdin and pipe it in");
    }
    let first = rpassword::prompt_password("Password (at least 12 characters): ")?;
    let second = rpassword::prompt_password("Repeat the password: ")?;
    if first != second {
        bail!("the passwords do not match");
    }
    Ok(first)
}

pub async fn create_admin(cfg: &DatabaseConfig, args: CreateAdminArgs) -> anyhow::Result<()> {
    let username_ok =
        regex::Regex::new(crate::api::schemas::USERNAME_PATTERN).is_ok_and(|re| re.is_match(&args.username));
    if !username_ok {
        bail!(
            "--username: letters, digits, \".\", \"_\", \"@\" and \"-\", starting with a letter or digit (max 64 characters)"
        );
    }
    let password = read_password(args.password_stdin)?;
    if let Some(problem) = crate::auth::password::policy_error(&password) {
        bail!("password: {problem}");
    }
    let pool = db::connect(cfg).await?;
    let result = async {
        if db::applied_count(&pool).await? != db::expected_count() {
            bail!("the database is not fully migrated; run `shadoucmdb migrate` first");
        }
        let ctx = RequestContext::system("cli: create-admin", "cli");
        let mut tx = pool.begin().await?;
        let admin = data::builtin_profile_id(&mut tx).await?;
        let input = UserCreate {
            display_name: args.display_name.clone().unwrap_or_else(|| args.username.clone()).trim().to_owned(),
            username: args.username.clone(),
            email: args.email.clone(),
            password,
            is_active: Some(true),
            profile_ids: vec![admin],
        };
        let user = users::create_in(&mut tx, &ctx, &input).await.map_err(|e| {
            if e.code == crate::http::error::ErrorCode::Conflict {
                return anyhow::anyhow!("a user named \"{}\" already exists", args.username);
            }
            let details: Vec<String> =
                e.details.iter().flatten().map(|d| format!("{}: {}", d.field, d.message)).collect();
            if details.is_empty() {
                anyhow::anyhow!("{}", e.message)
            } else {
                anyhow::anyhow!("{} ({})", e.message, details.join("; "))
            }
        })?;
        tx.commit().await?;
        println!("Created administrator \"{}\" ({})", user.username, user.id);
        Ok(())
    }
    .await;
    pool.close().await;
    result
}
