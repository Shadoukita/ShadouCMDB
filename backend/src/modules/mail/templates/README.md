# Workflow e-mail: copy conventions

These files are the text of every e-mail ShadouCMDB sends (v0.4.0 design SHAA-2725 §6.4). They are compiled
into the binary; a change here needs a rebuild.

| File | Content |
|------|---------|
| `en.txt`, `de.txt` | The message catalogues: one `key = text` per line, `{name}` placeholders. Both files must have the same keys and placeholders (a unit test in `render.rs` checks it). |
| `message.txt` | The plain-text layout. |
| `message.html` | The HTML layout. |

Every message is multipart (plain text and HTML). Apply these rules to every key and to both languages.

## Content

- **Subject.** It starts with the CI's ident in brackets, for example `[SRV-0042] Approved: Decommission for db01`.
  A message with `minimal` content, a bulk message and a digest name no CI and have no ident. The server
  removes line breaks and other control characters from every value in a subject and cuts it at 200 characters,
  so a CI label cannot add a header.
- **First sentence.** It says what happened and what, if anything, the reader must do ("No action is needed.",
  "please review the request and decide").
- **One link.** It is built from `PUBLIC_URL`, never from a request's `Host` header. The server refuses to start
  with `MAIL=smtp` and no `PUBLIC_URL`. There are no approve or reject links: a decision needs a signed-in
  session, and a one-click link would be a token in a mailbox.
- **Footer.** It says why the reader got the message (the `why.*` keys: "you are a member of the group CAB") and
  who to contact (the administrators). There is no unsubscribe link in v0.4.0: these are workflow duties, not
  newsletters (decision N-Q7).
- **What the reader may see.** The server renders each message for one recipient. A CI the recipient may not
  view is never named; a reference to one renders as `hidden_ref` ("a CI you cannot view"). A fixed address
  (`address` recipient) only ever gets `minimal` content (decision N-Q3). Do not add a placeholder that carries
  CI data to a `*.minimal` key.

## Wording

- Neutral, professional wording for IT staff. No exclamation marks, no emoji, no marketing tone.
- German addresses the reader with "Sie".
- Use the terms of the web UI (`frontend/src/i18n/{en,de}.ts`): Configuration item / Konfigurationselement,
  Workflow, Transition, Approval / Genehmigung, State / Status, Permission profile / Berechtigungsprofil.
- Names chosen by administrators (workflows, transitions, states, groups, fields) are inserted as they are and
  are not translated.
- Times are UTC and say so.

## Layout

- Inline CSS only. No remote images, no tracking pixels, no web fonts, no scripts: the message must render in
  Outlook and on clients without internet access.
- Every value is HTML-escaped in the HTML part. Do not put a placeholder inside an HTML attribute other than
  the link `href`, which the server builds.

## Headers

Set by the server, not by the templates:

- `Auto-Submitted: auto-generated` (RFC 3834) and `X-Auto-Response-Suppress: All`, so out-of-office replies do
  not answer back.
- A `Message-ID` that stays the same across the retries of one delivery, with `PUBLIC_URL`'s host on the right,
  so a relay that received a message before a lost reply can drop the duplicate.
- `Reply-To` from `MAIL_REPLY_TO` when set.

## Adding a message

1. Add the key to `en.txt` and `de.txt` with the same placeholders.
2. Use it from `render.rs`.
3. Run `cargo test --locked mail::render` (the catalogue test) and the e-mail tests in
   `workflows::actions::email_tests` against a real PostgreSQL.
