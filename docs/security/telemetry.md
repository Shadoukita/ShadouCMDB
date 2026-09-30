# Telemetry and offline operation

**ShadouCMDB sends nothing to us.** It has no telemetry, usage statistics, crash reporting,
licence check, update check or other "phone home" of any kind, and it works on a network with no
internet access.

## What the server connects to

`shadoucmdb` opens only these outbound connections. Every destination is one you configure;
apart from PostgreSQL, each is off until an administrator sets it up:

| Destination | When | Enabled by | Protocol and TLS |
| --- | --- | --- | --- |
| Your PostgreSQL server | always | `DATABASE_URL` or `PG*` | PostgreSQL protocol; TLS per `DATABASE_SSL` (`verify-full` recommended, see [deployment](../deployment.md)) |
| Your OIDC identity provider | when an OIDC provider is enabled: discovery and key set (cached for an hour), and the token request at each sign-in; also the administrator's connection test | an OIDC provider under **Administration › Identity providers** (API: [Enterprise sign-in](../api.md#enterprise-sign-in)) | HTTPS only (plain HTTP is accepted only for a loopback test issuer); certificates always verified; no redirects followed; honours `HTTPS_PROXY`/`NO_PROXY`; `OIDC_ALLOWED_HOSTS` restricts the hosts it may contact |
| Your LDAP / Active Directory server | at each directory sign-in, and the administrator's connection test | an LDAP provider under **Administration › Identity providers** | `ldaps://`, or `ldap://` with StartTLS only; certificates always verified |
| Your SIEM or log collector | continuously, as audit rows are written | `AUDIT_EXPORT=udp://host:port` or `tcp://host:port` (see [deployment](../deployment.md#hardening-settings)) | syslog (RFC 5424) or JSON lines over UDP or TCP, **without TLS**: for a remote collector, send to a local relay that adds TLS |

The OIDC and LDAP destinations are the `oidc.issuerUrl` and `ldap.url` stored on each provider (and, for
OIDC, the `token_endpoint` and `jwks_uri` its discovery document names; the browser, not the
server, goes to the authorization endpoint). The server resolves these host names through the
operating system's DNS resolver. See [enterprise sign-in](hardening.md#enterprise-sign-in) for the
egress firewall rules.

That is the complete list. The server makes no connection to the ShadouCMDB project or any other
vendor or third-party endpoint: its only HTTP client is the one that calls the OIDC provider you
configure. Logs go to stdout or the file you name with `--log-file`, and `AUDIT_EXPORT=stdout` or
`file:<path>` stays on the host; where they go from there is up to you.

## What the browser connects to

The web UI and Swagger UI (`/docs`) load every script, style, font and image from the ShadouCMDB
server itself. No CDN, web font service, analytics or third-party script is used, and the
Content-Security-Policy (`default-src 'self'; connect-src 'self'`) stops the browser from loading
or sending anything elsewhere, even if the UI were tampered with.

The one exception you can switch on is `CSP_REPORT_URI`: browsers then send
Content-Security-Policy violation reports to the address you set. It is off by default, and the
reports go only to your collector.

## Planned features that will connect out

These are not built yet. When they ship, each will be off until an administrator configures it,
and this page will list it:

- **Discovery** (scanning your network, or agents reporting to the server);
- **Webhooks** (the server calling URLs you register).

See their [security requirements](feature-requirements.md). None of them will contact the
ShadouCMDB project.

## Offline and air-gapped installation

Everything the server needs is in the release: the binary embeds the web UI and the database
migrations. To install without internet access, download the release archive (or `docker save`
the image) and `SHA256SUMS` on a connected machine, verify, and copy both across. Updates work
the same way; the product never fetches them itself (see [support period](support-period.md#how-updates-reach-you)).

## Personal data

The personal data ShadouCMDB stores stays in your PostgreSQL database: user accounts (username,
display name, optional email), sessions with the client IP, and an audit log of changes and
sign-in events with the actor, client IP and user agent (see [api.md](../api.md), *Audit log*).
You are the controller for that data.
