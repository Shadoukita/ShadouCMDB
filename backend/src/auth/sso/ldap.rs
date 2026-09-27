//! LDAP / Active Directory sign-in: search the user with the service account,
//! then bind as them with the password they typed.
//!
//! - Only `ldaps://` or `ldap://` with StartTLS (the schema refuses anything
//!   else), certificates always verified ([`super::tls`]). The password never
//!   crosses the network in clear.
//! - An empty password is refused before any bind: LDAP servers treat a simple
//!   bind with an empty password as an anonymous bind and answer "success"
//!   (RFC 4513 §5.1.2).
//! - The sign-in name is escaped into the search filter (RFC 4515), and the
//!   search must find exactly one entry.
//! - Groups are the values of the group attribute (`memberOf`: direct
//!   membership; for Active Directory nested groups use a filter with
//!   LDAP_MATCHING_RULE_IN_CHAIN in the directory, or map each group).
//! - The account's stable id is the entry's objectGUID (AD) or entryUUID
//!   (OpenLDAP, 389-DS), else its DN in lower case.

use std::sync::Arc;
use std::time::Duration;

use ldap3::{Ldap, LdapConnAsync, LdapConnSettings, LdapError, Scope, SearchEntry, ldap_escape};

use super::tls;

/// Timeout for connecting and for each operation.
const TIMEOUT: Duration = Duration::from_secs(10);
/// LDAP result code invalidCredentials.
const INVALID_CREDENTIALS: u32 = 49;

#[derive(Debug, Clone)]
pub struct Settings {
    pub url: String,
    pub start_tls: bool,
    pub bind_dn: Option<String>,
    pub bind_password: Option<String>,
    pub user_base_dn: String,
    pub user_filter: String,
    pub username_attribute: String,
    pub display_name_attribute: String,
    pub email_attribute: String,
    pub group_attribute: String,
    pub ca_certificate: Option<String>,
}

/// A user as the directory describes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryUser {
    pub dn: String,
    pub external_id: String,
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub groups: Vec<String>,
}

#[derive(Debug)]
pub enum Outcome {
    /// No entry matches the name.
    NotFound,
    /// Several entries match: the filter is too broad; refused.
    Ambiguous(usize),
    WrongPassword,
    SignedIn(DirectoryUser),
}

/// The directory could not be reached or refused the service account. The
/// text is for the log and the administrator's connection test.
#[derive(Debug, Clone)]
pub struct DirectoryError(pub String);

impl std::fmt::Display for DirectoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn fail(what: &str, e: LdapError) -> DirectoryError {
    DirectoryError(format!("{what}: {e}"))
}

/// The configured filter with `{username}` replaced by the escaped name.
pub fn user_filter(template: &str, username: &str) -> String {
    template.replace("{username}", &ldap_escape(username))
}

async fn connect(s: &Settings) -> Result<Ldap, DirectoryError> {
    let config =
        tls::client_config(s.ca_certificate.as_deref()).map_err(|e| DirectoryError(format!("CA certificate: {e}")))?;
    let settings =
        LdapConnSettings::new().set_conn_timeout(TIMEOUT).set_starttls(s.start_tls).set_config(Arc::new(config));
    let (conn, ldap) = LdapConnAsync::with_settings(settings, &s.url).await.map_err(|e| fail("connection", e))?;
    tokio::spawn(async move {
        if let Err(e) = conn.drive().await {
            tracing::debug!(error = %e, "LDAP connection ended");
        }
    });
    Ok(ldap)
}

/// Binds with the service account, if one is configured.
async fn service_bind(ldap: &mut Ldap, s: &Settings) -> Result<(), DirectoryError> {
    if let (Some(dn), Some(pw)) = (&s.bind_dn, &s.bind_password) {
        ldap.with_timeout(TIMEOUT)
            .simple_bind(dn, pw)
            .await
            .and_then(|r| r.success())
            .map_err(|e| fail("service account bind", e))?;
    }
    Ok(())
}

fn first(entry: &SearchEntry, attribute: &str) -> Option<String> {
    all(entry, attribute).into_iter().next().filter(|v| !v.trim().is_empty())
}

/// Attribute names are case-insensitive; servers answer in their own spelling.
fn all(entry: &SearchEntry, attribute: &str) -> Vec<String> {
    entry.attrs.iter().find(|(k, _)| k.eq_ignore_ascii_case(attribute)).map(|(_, v)| v.clone()).unwrap_or_default()
}

fn external_id(entry: &SearchEntry) -> String {
    let binary = |name: &str| {
        entry.bin_attrs.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).and_then(|(_, v)| v.first().cloned())
    };
    if let Some(guid) = binary("objectGUID").filter(|g| g.len() == 16) {
        return format!("objectGUID:{}", hex::encode(guid));
    }
    if let Some(uuid) = first(entry, "entryUUID") {
        return format!("entryUUID:{}", uuid.to_ascii_lowercase());
    }
    format!("dn:{}", entry.dn.to_lowercase())
}

fn describe(entry: SearchEntry, s: &Settings) -> DirectoryUser {
    DirectoryUser {
        external_id: external_id(&entry),
        username: first(&entry, &s.username_attribute),
        display_name: first(&entry, &s.display_name_attribute),
        email: first(&entry, &s.email_attribute),
        groups: all(&entry, &s.group_attribute),
        dn: entry.dn,
    }
}

/// Searches the user; `Ok(Err(n))` when `n` != 1 entries match.
async fn find(ldap: &mut Ldap, s: &Settings, username: &str) -> Result<Result<DirectoryUser, usize>, DirectoryError> {
    let filter = user_filter(&s.user_filter, username);
    let attrs = vec![
        s.username_attribute.as_str(),
        &s.display_name_attribute,
        &s.email_attribute,
        &s.group_attribute,
        "objectGUID",
        "entryUUID",
    ];
    let (entries, _) = ldap
        .with_timeout(TIMEOUT)
        .search(&s.user_base_dn, Scope::Subtree, &filter, attrs)
        .await
        .and_then(|r| r.success())
        .map_err(|e| fail("user search", e))?;
    // search() returns entries only: the referrals Active Directory adds when
    // searching from the domain root (DomainDnsZones, ...) go to the result's
    // refs and are not followed.
    let mut users: Vec<SearchEntry> = entries.into_iter().map(SearchEntry::construct).collect();
    if users.len() != 1 {
        return Ok(Err(users.len()));
    }
    Ok(Ok(describe(users.remove(0), s)))
}

/// Checks `username` and `password` against the directory.
pub async fn authenticate(s: &Settings, username: &str, password: &str) -> Result<Outcome, DirectoryError> {
    if password.is_empty() {
        return Ok(Outcome::WrongPassword);
    }
    let mut ldap = connect(s).await?;
    service_bind(&mut ldap, s).await?;
    let user = match find(&mut ldap, s, username).await? {
        Ok(user) => user,
        Err(0) => return Ok(Outcome::NotFound),
        Err(n) => return Ok(Outcome::Ambiguous(n)),
    };
    let outcome = match ldap.with_timeout(TIMEOUT).simple_bind(&user.dn, password).await {
        Ok(r) if r.rc == 0 => Outcome::SignedIn(user),
        Ok(r) if r.rc == INVALID_CREDENTIALS => Outcome::WrongPassword,
        Ok(r) => return Err(DirectoryError(format!("user bind: result code {} {}", r.rc, r.text))),
        Err(e) => return Err(fail("user bind", e)),
    };
    let _ = ldap.unbind().await;
    Ok(outcome)
}

/// For the administrator's connection test: connects, binds the service
/// account and, given a name, looks it up (no password involved).
pub async fn probe(
    s: &Settings,
    username: Option<&str>,
) -> Result<Option<Result<DirectoryUser, usize>>, DirectoryError> {
    let mut ldap = connect(s).await?;
    service_bind(&mut ldap, s).await?;
    let found = match username {
        Some(name) => Some(find(&mut ldap, s, name).await?),
        None => None,
    };
    let _ = ldap.unbind().await;
    Ok(found)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn the_name_is_escaped_into_the_filter() {
        assert_eq!(
            user_filter("(&(objectClass=user)(sAMAccountName={username}))", "alice"),
            "(&(objectClass=user)(sAMAccountName=alice))"
        );
        assert_eq!(user_filter("(uid={username})", "*)(uid=*"), "(uid=\\2a\\29\\28uid=\\2a)");
        assert_eq!(user_filter("(uid={username})", "a\\b\0"), "(uid=a\\5cb\\00)");
    }

    fn entry(attrs: &[(&str, &[&str])], bin: &[(&str, Vec<u8>)]) -> SearchEntry {
        SearchEntry {
            dn: "CN=Alice,OU=Staff,DC=Example,DC=Test".into(),
            attrs: attrs.iter().map(|(k, v)| ((*k).to_owned(), v.iter().map(|s| (*s).to_owned()).collect())).collect(),
            bin_attrs: bin.iter().map(|(k, v)| ((*k).to_owned(), vec![v.clone()])).collect::<HashMap<_, _>>(),
        }
    }

    fn settings() -> Settings {
        Settings {
            url: "ldaps://dc.example.test".into(),
            start_tls: false,
            bind_dn: None,
            bind_password: None,
            user_base_dn: "DC=example,DC=test".into(),
            user_filter: "(sAMAccountName={username})".into(),
            username_attribute: "sAMAccountName".into(),
            display_name_attribute: "displayName".into(),
            email_attribute: "mail".into(),
            group_attribute: "memberOf".into(),
            ca_certificate: None,
        }
    }

    #[test]
    fn entries_are_read_case_insensitively_with_a_stable_id() {
        let e = entry(
            &[
                ("samaccountname", &["alice"]),
                ("DISPLAYNAME", &["Alice A."]),
                ("memberof", &["CN=CMDB,DC=x", "CN=All,DC=x"]),
            ],
            &[("objectGUID", vec![0xab; 16])],
        );
        let u = describe(e, &settings());
        assert_eq!(u.username.as_deref(), Some("alice"));
        assert_eq!(u.display_name.as_deref(), Some("Alice A."));
        assert_eq!(u.email, None);
        assert_eq!(u.groups, ["CN=CMDB,DC=x", "CN=All,DC=x"]);
        assert_eq!(u.external_id, format!("objectGUID:{}", "ab".repeat(16)));

        let e = entry(&[("entryUUID", &["3F2504E0-4F89-11D3-9A0C-0305E82C3301"])], &[]);
        assert_eq!(external_id(&e), "entryUUID:3f2504e0-4f89-11d3-9a0c-0305e82c3301");
        assert_eq!(external_id(&entry(&[], &[])), "dn:cn=alice,ou=staff,dc=example,dc=test");
    }

    #[tokio::test]
    async fn an_empty_password_never_reaches_the_directory() {
        // The URL points nowhere: an attempt to connect would fail, not "sign in".
        let s = Settings { url: "ldaps://unreachable.invalid".into(), ..settings() };
        assert!(matches!(authenticate(&s, "alice", "").await, Ok(Outcome::WrongPassword)));
    }
}
