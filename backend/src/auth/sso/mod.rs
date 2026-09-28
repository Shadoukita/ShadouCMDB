//! Sign-in through an identity provider: OpenID Connect ([`oidc`]) and LDAP /
//! Active Directory ([`ldap`]), both over verified TLS ([`tls`]). The accounts
//! and permission profiles they lead to are handled by
//! [`crate::modules::sso`]; local accounts stay available next to them.

pub mod jose;
pub mod ldap;
pub mod login_state;
pub mod oidc;
pub mod tls;
