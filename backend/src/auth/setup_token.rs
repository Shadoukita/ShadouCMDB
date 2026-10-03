//! The one-time first-run setup token (GitHub #192, risk-assessment T14).
//!
//! While no user exists, `POST /api/v1/setup` creates an administrator. Without
//! a token, whoever reaches a fresh install first would own it. The token is
//! required in that request, and only the operator can read it: from the token
//! file (`SETUP_TOKEN_FILE`, mode 0600), from the server log when there is no
//! token file or it cannot be written, or because they chose it themselves
//! (`SETUP_TOKEN`). A token written to the file never goes to the log, which
//! more people read than the service account (GH#436).
//!
//! The token is armed when this process first sees a database without users
//! (at start, or on the first `/api/v1/setup` request if the database was not
//! reachable then) and disarmed once setup has created the first administrator.
//! An installed system never generates one. A generated token lives only in
//! this process: after a restart a new one is generated and the file rewritten,
//! and each of several API processes has its own. Set `SETUP_TOKEN` to give
//! them all the same one.
//!
//! A wrong or missing token is logged at WARN, but at most [`REFUSALS_LOGGED`]
//! times per [`REFUSAL_LOG_INTERVAL`]; further refusals in that interval are
//! only counted, and the count is logged in one line when the next interval
//! starts (or setup completes). So a client looping wrong tokens cannot rotate
//! the line with the generated token out of a size-limited log (GH#230).

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

use super::secret::Secret;
use super::session::{constant_time_eq, new_token, token_hash};

/// `SETUP_TOKEN` shorter than this is refused at start.
pub const MIN_PRESET_LENGTH: usize = 32;

/// Refused setup requests logged one by one per [`REFUSAL_LOG_INTERVAL`].
pub const REFUSALS_LOGGED: u32 = 10;
pub const REFUSAL_LOG_INTERVAL: Duration = Duration::from_secs(60);

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
    refusals: Mutex<Refusals>,
}

/// Refusals in the current [`REFUSAL_LOG_INTERVAL`].
#[derive(Default)]
struct Refusals {
    since: Option<Instant>,
    logged: u32,
    suppressed: u64,
}

impl Refusals {
    /// Logs how many refusals went unlogged, if any, and starts over.
    fn flush(&mut self) {
        if self.suppressed > 0 {
            tracing::warn!(
                suppressed = self.suppressed,
                interval_secs = REFUSAL_LOG_INTERVAL.as_secs(),
                "first-run setup refused {} more times (wrong or missing setup token) without a log line each",
                self.suppressed
            );
        }
        *self = Refusals::default();
    }
}

impl SetupGate {
    pub fn new(preset: Option<Secret>, file: Option<PathBuf>) -> Self {
        SetupGate { preset, file, armed: Mutex::new(None), refusals: Mutex::default() }
    }

    /// Logs a request refused for a wrong or missing token, within the
    /// [`REFUSALS_LOGGED`] per [`REFUSAL_LOG_INTERVAL`]; counts the rest.
    pub fn refused(&self, client_ip: Option<IpAddr>) {
        let now = Instant::now();
        let mut r = self.refusals.lock().unwrap_or_else(|e| e.into_inner());
        if r.since.is_none_or(|since| now.duration_since(since) >= REFUSAL_LOG_INTERVAL) {
            r.flush();
            r.since = Some(now);
        }
        if r.logged >= REFUSALS_LOGGED {
            r.suppressed += 1;
            return;
        }
        r.logged += 1;
        let last = r.logged == REFUSALS_LOGGED;
        drop(r);
        if last {
            tracing::warn!(
                client_ip = ?client_ip,
                "first-run setup refused: the setup token is missing or wrong. Further refusals in the next {} s are counted, not logged",
                REFUSAL_LOG_INTERVAL.as_secs()
            );
        } else {
            tracing::warn!(client_ip = ?client_ip, "first-run setup refused: the setup token is missing or wrong");
        }
    }

    /// Refusals reported in the current interval, logged or not.
    #[cfg(test)]
    pub fn refusals(&self) -> u64 {
        let r = self.refusals.lock().unwrap();
        u64::from(r.logged) + r.suppressed
    }

    /// Arms the token unless it already is: takes `SETUP_TOKEN`, or generates
    /// one and writes it to the token file, or to the log when there is no
    /// file or it cannot be written. Call only while no user exists.
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
        let written = self.file.as_deref().filter(|path| match write_token_file(path, &token) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot write the setup token file; the token is only in this log");
                false
            }
        });
        // Whoever holds the token creates the first administrator, and log
        // readers are many more than the service account (GH#436): the token
        // goes to the log only when it is nowhere else.
        match written {
            Some(path) => tracing::warn!(
                file = %path.display(),
                "no user exists yet: complete first-run setup in the web UI with the one-time setup token in the setup \
                 token file. It is valid until the first administrator is created or this process stops"
            ),
            None => tracing::warn!(
                setup_token = %token,
                "no user exists yet: complete first-run setup in the web UI with this one-time setup token. It is valid \
                 until the first administrator is created or this process stops"
            ),
        }
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
        self.refusals.lock().unwrap_or_else(|e| e.into_inner()).flush();
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

/// Captures log lines in tests.
#[cfg(test)]
pub(crate) mod capture {
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    pub struct Lines(Arc<Mutex<Vec<u8>>>);

    impl Lines {
        pub fn lines(&self) -> Vec<String> {
            String::from_utf8_lossy(&self.0.lock().unwrap()).lines().map(str::to_owned).collect()
        }

        /// Lines containing `needle`.
        pub fn count(&self, needle: &str) -> usize {
            self.lines().iter().filter(|l| l.contains(needle)).count()
        }
    }

    impl std::io::Write for Lines {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Lines {
        type Writer = Lines;

        fn make_writer(&'a self) -> Lines {
            self.clone()
        }
    }

    /// WARN and above logged on this thread until the guard is dropped.
    pub fn warnings() -> (Lines, tracing::subscriber::DefaultGuard) {
        let lines = Lines::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(lines.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .without_time()
            .finish();
        (lines, install(subscriber))
    }

    /// INFO and above, as the JSON lines `logging::init` writes (with the
    /// request span's fields), logged on this thread until the guard is dropped.
    pub fn json() -> (Lines, tracing::subscriber::DefaultGuard) {
        let lines = Lines::default();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_writer(lines.clone())
            .with_max_level(tracing::Level::INFO)
            .with_current_span(true)
            .with_span_list(false)
            .without_time()
            .finish();
        (lines, install(subscriber))
    }

    fn install(subscriber: impl tracing::Subscriber + Send + Sync + 'static) -> tracing::subscriber::DefaultGuard {
        // With one scoped subscriber registered, a call site first reached on
        // another thread takes that thread's interest (none) for good; a
        // second one, kept alive, makes tracing ask every live subscriber.
        static SECOND: std::sync::LazyLock<tracing::Dispatch> =
            std::sync::LazyLock::new(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
        std::sync::LazyLock::force(&SECOND);
        let guard = tracing::subscriber::set_default(subscriber);
        // Interest is cached per call site for all threads; a test registering
        // its subscriber at the same time could leave this one's out.
        tracing::callsite::rebuild_interest_cache();
        guard
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFUSED: &str = "first-run setup refused: the setup token is missing or wrong";
    const SUMMARY: &str = "more times (wrong or missing setup token)";

    /// GH#230: a client looping wrong tokens gets [`REFUSALS_LOGGED`] lines
    /// per interval, then one line with the count.
    #[tokio::test(start_paused = true)]
    async fn refusals_are_logged_up_to_the_cap_then_counted() {
        let (log, _guard) = capture::warnings();
        let gate = SetupGate::new(Some("operator-chosen-setup-token-0123456789".into()), None);
        let extra = 7;
        for _ in 0..REFUSALS_LOGGED + extra {
            gate.refused(Some([192, 0, 2, 1].into()));
        }
        assert_eq!(log.count(REFUSED), REFUSALS_LOGGED as usize);
        assert_eq!(log.count("Further refusals in the next 60 s are counted"), 1, "the last one says so");
        assert_eq!(log.count(SUMMARY), 0, "counted until the interval ends");

        tokio::time::advance(REFUSAL_LOG_INTERVAL - Duration::from_millis(1)).await;
        gate.refused(None);
        assert_eq!(log.count(SUMMARY), 0, "still the same interval");

        tokio::time::advance(Duration::from_millis(1)).await;
        gate.refused(None);
        let summary: Vec<_> = log.lines().into_iter().filter(|l| l.contains(SUMMARY)).collect();
        assert_eq!(summary.len(), 1);
        assert!(summary[0].contains(&format!("suppressed={}", extra + 1)), "{}", summary[0]);
        assert_eq!(log.count(REFUSED), REFUSALS_LOGGED as usize + 1, "a new interval logs again");

        // Setup completing reports what the current interval counted.
        for _ in 0..REFUSALS_LOGGED {
            gate.refused(None);
        }
        gate.disarm();
        assert!(log.lines().last().unwrap().contains("suppressed=1"), "{:?}", log.lines().last());
        assert_eq!(log.count(REFUSED), 2 * REFUSALS_LOGGED as usize);
        assert_eq!(log.count(SUMMARY), 2);
    }

    #[test]
    fn a_generated_token_is_written_0600_and_removed_on_disarm() {
        let dir = std::env::temp_dir().join(format!("shadoucmdb-setup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("setup-token");
        // A stale file with wider permissions is replaced, not reused.
        std::fs::write(&path, "stale\n").unwrap();

        let (log, guard) = capture::warnings();
        let gate = SetupGate::new(None, Some(path.clone()));
        assert!(!gate.matches("stale"), "nothing is armed yet");
        gate.arm();
        drop(guard);
        let token = std::fs::read_to_string(&path).unwrap().trim().to_owned();
        assert_eq!(token.len(), 64);
        // GH#436: the log names the file, never the token in it.
        assert_eq!(log.count(&token), 0, "{:?}", log.lines());
        assert_eq!(log.count("setup_token="), 0, "{:?}", log.lines());
        assert_eq!(log.count(&format!("file={}", path.display())), 1, "{:?}", log.lines());
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

    /// Without a token file, or when it cannot be written, the log is the only
    /// place the operator can read a generated token from.
    #[test]
    fn a_generated_token_is_logged_only_when_no_file_holds_it() {
        let unwritable =
            std::env::temp_dir().join(format!("shadoucmdb-missing-{}", uuid::Uuid::new_v4())).join("setup-token");
        for file in [None, Some(unwritable.clone())] {
            let (log, guard) = capture::warnings();
            let gate = SetupGate::new(None, file.clone());
            gate.arm();
            drop(guard);
            let line = log.lines().into_iter().find(|l| l.contains("setup_token=")).expect("token logged");
            let token = line.split("setup_token=").nth(1).unwrap().split_whitespace().next().unwrap().to_owned();
            assert_eq!(token.len(), 64, "{line}");
            assert!(gate.matches(&token));
            if file.is_some() {
                assert_eq!(log.count("cannot write the setup token file; the token is only in this log"), 1);
                assert!(!unwritable.exists());
            }
        }
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
