# Telemetry and offline operation

**ShadouCMDB sends nothing to us.** It has no telemetry, usage statistics, crash reporting,
licence check, update check or other "phone home" of any kind, and it works on a network with no
internet access.

## What the server connects to

`shadoucmdb` opens exactly these outbound connections, all configured by you:

| Destination | When | Configured by |
| --- | --- | --- |
| Your PostgreSQL server | always | `DATABASE_URL` or `PG*` |

That is the complete list: the server contains no HTTP client. Logs go to stdout or the file you
name with `--log-file`; where they go from there is up to you.

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
