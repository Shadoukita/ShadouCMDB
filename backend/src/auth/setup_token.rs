//! The one-time first-run setup token (GitHub #192, risk-assessment T14).
//!
//! While no user exists, `POST /api/v1/setup` creates an administrator. Without
//! a token, whoever reaches a fresh install first would own it. The token is
//! required in that request, and only the operator can read it: from the server
//! log, from the token file (`SETUP_TOKEN_FILE`, mode 0600), or because they
//! chose it themselves (`SETUP_TOKEN`).
//!
//! The token is armed when this process first sees a database without users
//! (at start, or on the first `/api/v1/setup` request if the database was not
//! reachable then) and disarmed once setup has created the first administrator.
//! An installed system never generates one. A generated token lives only in
//! this process: after a restart a new one is generated and the file rewritten,
//! and each of several API processes has its own. Set `SETUP_TOKEN` to give
//! them all the same one.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::secret::Secret;
use super::session::{constant_time_eq, new_token, token_hash};

/// `SETUP_TOKEN` shorter than this is refused at start.
pub const MIN_PRESET_LENGTH: usize = 32;

/// The `SETUP_TOKEN` of the test routers and states.
#[cfg(test)]
pub const TEST_TOKEN: &str = "test-setup-token-0123456789abcdef";

pub struct SetupGate {
    /// `SETUP_TOKEN`, when the operator chose the token.
    preset: Option<Secret>,
    /// Where a generated token is written (`SETUP_TOKEN_FILE`, or next to the env file).
    file: Option<PathBuf>,
    /// SHA-256 of the armed token; None while setup is not open in this process.
    armed: Mutex<Option<Vec<u8>>>,
}

impl SetupGate {
    pub fn new(preset: Option<Secret>, file: Option<PathBuf>) -> Self {
        SetupGate { preset, file, armed: Mutex::new(None) }
    }

    /// Arms the token unless it already is: takes `SETUP_TOKEN`, or generates
    /// one and writes it to the log and the token file. Call only while no user exists.
    pub fn arm(&self) {
        let mut armed = self.armed.lock().expect("setup gate lock");
        if armed.is_some() {
            return;
        }
        if let Some(preset) = &self.preset {
            *armed = Some(token_hash(preset));
            tracing::warn!(
                "no user exists yet: complete first-run setup in the web UI with the setup token set in SETUP_TOKEN"
            );
            return;
        }
        let token = new_token();
        *armed = Some(token_hash(&token));
        let file = self.file.as_deref().and_then(|path| match write_token_file(path, &token) {
            Ok(()) => Some(path.display().to_string()),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot write the setup token file; the token is only in this log");
                None
            }
        });
        tracing::warn!(
            setup_token = %token,
            file = file.as_deref().unwrap_or("none"),
            "no user exists yet: complete first-run setup in the web UI with this one-time setup token. It is valid \
             until the first administrator is created or this process stops"
        );
    }

    /// Whether `sent` is the armed token. False while none is armed.
    pub fn matches(&self, sent: &str) -> bool {
        let armed = self.armed.lock().expect("setup gate lock");
        armed.as_deref().is_some_and(|hash| constant_time_eq(&token_hash(sent), hash))
    }

    /// Ends first-run setup in this process: the token stops working and a
    /// generated token's file is deleted. An installed system calls this at start,
    /// which also removes a file a crashed earlier run left behind.
    pub fn disarm(&self) {
        *self.armed.lock().expect("setup gate lock") = None;
        if self.preset.is_none()
            && let Some(path) = &self.file
        {
            match std::fs::remove_file(path) {
                Ok(()) => tracing::info!(path = %path.display(), "deleted the setup token file"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!(path = %path.display(), error = %e, "cannot delete the setup token file"),
            }
        }
    }
}

/// Writes the token to a new file only its owner can read (0600 on Unix). A
/// file already there is replaced, never reused, so its old permissions do not carry over.
fn write_token_file(path: &Path, token: &str) -> std::io::Result<()> {
    use std::io::Write;
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    writeln!(file, "{token}")?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_token_is_written_0600_and_removed_on_disarm() {
        let dir = std::env::temp_dir().join(format!("shadoucmdb-setup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("setup-token");
        // A stale file with wider permissions is replaced, not reused.
        std::fs::write(&path, "stale\n").unwrap();

        let gate = SetupGate::new(None, Some(path.clone()));
        assert!(!gate.matches("stale"), "nothing is armed yet");
        gate.arm();
        let token = std::fs::read_to_string(&path).unwrap().trim().to_owned();
        assert_eq!(token.len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert!(gate.matches(&token));
        assert!(!gate.matches("stale"));
        assert!(!gate.matches(""));

        // Arming again keeps the same token.
        gate.arm();
        assert!(gate.matches(&token));

        gate.disarm();
        assert!(!gate.matches(&token), "one use: the token stops working");
        assert!(!path.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_preset_token_is_used_as_is_and_writes_no_file() {
        let dir = std::env::temp_dir().join(format!("shadoucmdb-setup-{}", uuid::Uuid::new_v4()));
        let preset = "operator-chosen-setup-token-0123456789";
        let gate = SetupGate::new(Some(preset.into()), Some(dir.join("setup-token")));
        gate.arm();
        assert!(gate.matches(preset));
        assert!(!gate.matches(&preset[1..]));
        assert!(!dir.exists());
        gate.disarm();
        assert!(!gate.matches(preset));
    }
}
