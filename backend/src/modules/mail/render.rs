//! The text of workflow e-mail: message catalogues in English and German
//! (`templates/{en,de}.txt`), one HTML and one plain-text layout
//! (`templates/message.{html,txt}`), all compiled into the binary. The copy
//! conventions are in `templates/README.md`.
//!
//! Everything here works on what one recipient may see: the caller (the
//! e-mail channel) leaves a CI out, or replaces a reference with "a CI you
//! cannot view", before it gets here. Every value is HTML-escaped in the HTML
//! part, and a subject never carries a line break or other control
//! character, so no value can add a header.

use std::collections::HashMap;
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use lettre::message::header::{ContentType, Header, HeaderName, HeaderValue};
use lettre::message::{Mailbox, MultiPart, SinglePart};
use uuid::Uuid;

use super::Mail;

/// The longest subject sent, in characters.
const MAX_SUBJECT: usize = 200;
/// CIs listed by label in a bulk message; the rest are counted.
pub const BULK_LIST: usize = 50;
/// Entries listed in a digest; the rest are counted.
pub const DIGEST_LIST: usize = 100;

/// The language of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Locale {
    En,
    De,
}

impl Locale {
    /// A user's language, else `MAIL_DEFAULT_LOCALE`.
    pub fn of(user: Option<&str>, default: &str) -> Locale {
        match user.unwrap_or(default) {
            "de" => Locale::De,
            _ => Locale::En,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Locale::En => "en",
            Locale::De => "de",
        }
    }
}

fn parse_catalogue(text: &'static str) -> HashMap<&'static str, &'static str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once(" = ").map(|(k, v)| (k.trim(), v.trim())))
        .collect()
}

static EN: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| parse_catalogue(include_str!("templates/en.txt")));
static DE: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| parse_catalogue(include_str!("templates/de.txt")));
const HTML_LAYOUT: &str = include_str!("templates/message.html");
const TEXT_LAYOUT: &str = include_str!("templates/message.txt");

/// Replaces each `open name close` in `template` with `value(name)` in one
/// pass, so a value that itself looks like a placeholder stays as it is.
fn fill(template: &str, open: &str, close: &str, value: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(template.len() + 64);
    let mut rest = template;
    while let Some(start) = rest.find(open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(close) else { break };
        let name = &after[..end];
        match value(name.trim()) {
            Some(v) => {
                out.push_str(&rest[..start]);
                out.push_str(&v);
            }
            None => out.push_str(&rest[..start + open.len() + end + close.len()]),
        }
        rest = &after[end + close.len()..];
    }
    out.push_str(rest);
    out
}

/// Catalogue text `key` in `locale` with `{name}` filled from `args`.
fn t(locale: Locale, key: &str, args: &[(&str, &str)]) -> String {
    let catalogue = match locale {
        Locale::En => &*EN,
        Locale::De => &*DE,
    };
    let text = catalogue.get(key).or_else(|| EN.get(key)).copied().unwrap_or(key);
    fill(text, "{", "}", &|name| args.iter().find(|(k, _)| *k == name).map(|(_, v)| (*v).to_owned()))
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// One line, no control characters (CR and LF above all), at most
/// `MAX_SUBJECT` characters: what a subject may be.
pub fn subject_line(s: &str) -> String {
    let spaced: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let mut one = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > MAX_SUBJECT {
        one = one.chars().take(MAX_SUBJECT - 1).collect::<String>() + "…";
    }
    one
}

/// A value in running text: one line.
fn inline(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// What a reference to a CI the recipient may not view reads.
pub fn hidden_reference(locale: Locale) -> String {
    t(locale, "hidden_ref", &[])
}

pub fn yes_no(locale: Locale, value: bool) -> String {
    t(locale, if value { "yes" } else { "no" }, &[])
}

pub fn format_time(locale: Locale, at: DateTime<Utc>) -> String {
    match locale {
        Locale::En => at.format("%Y-%m-%d %H:%M UTC").to_string(),
        Locale::De => at.format("%d.%m.%Y %H:%M UTC").to_string(),
    }
}

// ---------------------------------------------------------------------------
// What a message is about, as one recipient may see it
// ---------------------------------------------------------------------------

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Transition,
    ApprovalRequested,
    ApprovalStep,
    /// With the request's outcome: `approved`, `rejected`, `withdrawn` or `cancelled`.
    ApprovalClosed(&'static str),
    ApprovalOverdue,
    Cancelled,
    Forced,
}

impl EventKind {
    fn key(self) -> String {
        match self {
            EventKind::Transition => "transition".into(),
            EventKind::ApprovalRequested => "approval_requested".into(),
            EventKind::ApprovalStep => "approval_step".into(),
            EventKind::ApprovalClosed(s) => format!("approval_closed.{s}"),
            EventKind::ApprovalOverdue => "approval_overdue".into(),
            EventKind::Cancelled => "cancelled".into(),
            EventKind::Forced => "forced".into(),
        }
    }

    fn is_approval(self) -> bool {
        matches!(
            self,
            EventKind::ApprovalRequested
                | EventKind::ApprovalStep
                | EventKind::ApprovalClosed(_)
                | EventKind::ApprovalOverdue
        )
    }
}

/// A CI the recipient may view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ci {
    pub label: String,
    pub ident: Option<String>,
    /// The type's name.
    pub class: String,
}

impl Ci {
    /// The bracketed reference of a subject.
    fn reference(&self) -> &str {
        self.ident.as_deref().filter(|i| !i.is_empty()).unwrap_or(&self.label)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Approval {
    pub request_no: Option<i32>,
    pub step: Option<String>,
    pub due_at: Option<DateTime<Utc>>,
    pub requested_by: Option<String>,
}

/// One event for one recipient. `ci` is None in minimal content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub kind: EventKind,
    pub at: DateTime<Utc>,
    pub workflow: String,
    pub transition: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub actor: Option<String>,
    pub comment: Option<String>,
    pub ci: Option<Ci>,
    pub approval: Option<Approval>,
    /// The type's summary fields (`detailed` content), already redacted for the recipient.
    pub fields: Vec<(String, String)>,
    /// The instance or the approval request in the web UI.
    pub url: String,
}

/// How much a message tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    Minimal,
    Standard,
    Detailed,
}

/// Why a recipient gets a message.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Why {
    Profile(String),
    Group(String),
    User,
    CiOwner,
    /// The field's label.
    CiAttribute(String),
    /// The role: `technical` or `business`. The service is not named: the
    /// recipient may not be allowed to view business services.
    ServiceOwner(&'static str),
    Actor,
    Starter,
    Requester,
    Approver,
    /// On behalf of this user.
    Delegate(String),
    Address,
    /// The recipient sent a test of the action to themselves.
    Test,
}

fn why_text(locale: Locale, why: &Why, workflow: &str) -> String {
    match why {
        Why::Profile(name) => t(locale, "why.profile", &[("name", name)]),
        Why::Group(name) => t(locale, "why.group", &[("name", name)]),
        Why::User => t(locale, "why.user", &[]),
        Why::CiOwner => t(locale, "why.ci_owner", &[]),
        Why::CiAttribute(name) => t(locale, "why.ci_attribute", &[("name", name)]),
        Why::ServiceOwner(role) => t(locale, &format!("why.service_owner.{role}"), &[]),
        Why::Actor => t(locale, "why.actor", &[]),
        Why::Starter => t(locale, "why.starter", &[]),
        Why::Requester => t(locale, "why.requester", &[]),
        Why::Approver => t(locale, "why.approver", &[]),
        Why::Delegate(name) => t(locale, "why.delegate", &[("name", name)]),
        Why::Address => t(locale, "why.address", &[("workflow", workflow)]),
        Why::Test => t(locale, "why.test", &[]),
    }
}

/// The administrator's subject and intro for the recipient's language (the
/// other language when one is missing).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Custom {
    pub subject: Option<String>,
    pub intro: Option<String>,
}

/// A message's subject and its two bodies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub locale: Locale,
    pub subject: String,
    pub text: String,
    pub html: String,
}

/// The value of an administrator placeholder (`{{ci.label}}`) for one event:
/// the CI ones are empty in minimal content.
fn placeholder(locale: Locale, e: &Event, name: &str) -> Option<String> {
    let ci = e.ci.as_ref();
    let approval = e.approval.as_ref();
    Some(match name {
        "ci.label" => ci.map(|c| c.label.clone()).unwrap_or_default(),
        "ci.ident" => ci.and_then(|c| c.ident.clone()).unwrap_or_default(),
        "ci.class" => ci.map(|c| c.class.clone()).unwrap_or_default(),
        "workflow.name" => e.workflow.clone(),
        "transition.name" => e.transition.clone().unwrap_or_default(),
        "state.from" => e.from.clone().unwrap_or_default(),
        "state.to" => e.to.clone().unwrap_or_default(),
        "actor.name" => e.actor.clone().unwrap_or_default(),
        "approval.step" => approval.and_then(|a| a.step.clone()).unwrap_or_default(),
        "approval.dueAt" => approval.and_then(|a| a.due_at).map(|d| format_time(locale, d)).unwrap_or_default(),
        _ => return None,
    })
}

/// `text` (an administrator's subject or intro) with its placeholders filled.
pub fn custom_text(locale: Locale, e: &Event, text: &str) -> String {
    fill(text, "{{", "}}", &|name| placeholder(locale, e, name))
}

/// The pieces both layouts are filled from; text pieces are plain, `html`
/// pieces are escaped already.
struct Body {
    intro: Vec<String>,
    lead: String,
    details: Vec<(String, String)>,
    list_title: Option<String>,
    list: Vec<String>,
    link_text: String,
    link: String,
    footer: Vec<String>,
}

fn layout(locale: Locale, subject: String, b: Body) -> Rendered {
    let html_intro: String = b
        .intro
        .iter()
        .map(|p| format!("<p style=\"margin:0 0 16px 0;\">{}</p>", escape(p).replace('\n', "<br>")))
        .collect();
    let html_details = if b.details.is_empty() {
        String::new()
    } else {
        let rows: String = b
            .details
            .iter()
            .map(|(k, v)| {
                format!(
                    "<tr><td style=\"padding:4px 12px 4px 0;color:#5b636e;vertical-align:top;white-space:nowrap;\">\
                     {}</td><td style=\"padding:4px 0;vertical-align:top;\">{}</td></tr>",
                    escape(k),
                    escape(v).replace('\n', "<br>")
                )
            })
            .collect();
        format!(
            "<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" \
             style=\"margin:0 0 16px 0;font-size:14px;\">{rows}</table>"
        )
    };
    let html_list = if b.list.is_empty() {
        String::new()
    } else {
        let items: String =
            b.list.iter().map(|i| format!("<li style=\"margin:0 0 4px 0;\">{}</li>", escape(i))).collect();
        let title = b.list_title.as_deref().map(|t| format!("<p style=\"margin:0 0 8px 0;\">{}</p>", escape(t)));
        format!("{}<ul style=\"margin:0 0 16px 0;padding-left:20px;\">{items}</ul>", title.unwrap_or_default())
    };
    let html_footer: String =
        b.footer.iter().map(|p| format!("<p style=\"margin:0 0 8px 0;\">{}</p>", escape(p))).collect();
    let html = fill(HTML_LAYOUT, "{{", "}}", &|slot| {
        Some(match slot {
            "lang" => locale.as_str().to_owned(),
            "subject" => escape(&subject),
            "intro" => html_intro.clone(),
            "lead" => escape(&b.lead),
            "details" => html_details.clone(),
            "list" => html_list.clone(),
            "link" => escape(&b.link),
            "link_text" => escape(&b.link_text),
            "footer" => html_footer.clone(),
            _ => return None,
        })
    });

    let width = b.details.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0);
    let text_details: String = b
        .details
        .iter()
        .map(|(k, v)| format!("{k}:{}{}\n", " ".repeat(width + 2 - k.chars().count()), v.replace('\n', " ")))
        .collect();
    let text_details = if text_details.is_empty() { text_details } else { text_details + "\n" };
    let text_list = if b.list.is_empty() {
        String::new()
    } else {
        let title = b.list_title.as_deref().map(|t| format!("{t}\n")).unwrap_or_default();
        format!("{title}{}\n", b.list.iter().map(|i| format!("- {i}\n")).collect::<String>())
    };
    let text_intro: String = b.intro.iter().map(|p| format!("{p}\n\n")).collect();
    let text = fill(TEXT_LAYOUT, "{{", "}}", &|slot| {
        Some(match slot {
            "intro" => text_intro.clone(),
            "lead" => b.lead.clone(),
            "details" => text_details.clone(),
            "list" => text_list.clone(),
            "link" => b.link.clone(),
            "link_text" => b.link_text.clone(),
            "footer" => b.footer.join("\n"),
            _ => return None,
        })
    });
    Rendered { locale, subject: subject_line(&subject), text, html }
}

fn reasons(locale: Locale, why: &[Why], workflow: &str) -> String {
    let texts: Vec<String> = why.iter().map(|w| why_text(locale, w, workflow)).collect();
    let joined = if texts.is_empty() { t(locale, "why.workflows", &[]) } else { texts.join("; ") };
    t(locale, "why.intro", &[("reasons", &joined)])
}

fn footer(locale: Locale, why: &[Why], workflow: &str, action: &str) -> Vec<String> {
    vec![
        reasons(locale, why, workflow),
        t(locale, "footer.action", &[("action", action), ("workflow", workflow)]),
        t(locale, "footer.contact", &[]),
    ]
}

/// The arguments the catalogue texts of event `e` use.
fn event_args(locale: Locale, e: &Event) -> Vec<(&'static str, String)> {
    let someone = t(locale, "someone", &[]);
    let approval = e.approval.clone().unwrap_or_default();
    vec![
        ("ref", e.ci.as_ref().map(|c| inline(c.reference())).unwrap_or_default()),
        ("ci", e.ci.as_ref().map(|c| inline(&c.label)).unwrap_or_default()),
        ("workflow", inline(&e.workflow)),
        ("transition", inline(e.transition.as_deref().unwrap_or_default())),
        ("from", inline(e.from.as_deref().unwrap_or_default())),
        ("to", inline(e.to.as_deref().unwrap_or_default())),
        ("actor", e.actor.as_deref().map(inline).unwrap_or(someone)),
        ("step", approval.step.as_deref().map(inline).unwrap_or_default()),
    ]
}

fn targs<'a>(args: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    args.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

fn detail_rows(locale: Locale, e: &Event, content: Content) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    let mut row = |key: &str, value: String| rows.push((t(locale, key, &[]), value));
    if let Some(ci) = &e.ci {
        let ident = ci.ident.as_deref().map(|i| format!(" ({i})")).unwrap_or_default();
        row("label.ci", format!("{}{ident}", ci.label));
        row("label.type", ci.class.clone());
    }
    row("label.workflow", e.workflow.clone());
    if let Some(tr) = &e.transition {
        row("label.transition", tr.clone());
    }
    if content == Content::Minimal {
        return rows;
    }
    match (&e.from, &e.to) {
        (Some(f), Some(to)) if f != to => row("label.state", format!("{f} → {to}")),
        (_, Some(to)) => row("label.state", to.clone()),
        _ => {}
    }
    if let Some(a) = &e.actor {
        row("label.by", a.clone());
    }
    if let Some(a) = e.approval.as_ref().filter(|_| e.kind.is_approval()) {
        if let Some(no) = a.request_no {
            row("label.request", format!("#{no}"));
        }
        if let Some(by) = &a.requested_by {
            row("label.requested_by", by.clone());
        }
        if let Some(step) = &a.step {
            row("label.step", step.clone());
        }
        if let Some(due) = a.due_at {
            row("label.due", format_time(locale, due));
        }
    }
    if let Some(c) = e.comment.as_ref().filter(|c| !c.trim().is_empty()) {
        row("label.comment", c.clone());
    }
    if content == Content::Detailed {
        rows.extend(e.fields.iter().cloned());
    }
    rows
}

fn intro(locale: Locale, e: &Event, custom: &Custom) -> Vec<String> {
    custom
        .intro
        .as_deref()
        .map(|i| custom_text(locale, e, i))
        .map(|i| i.split("\n\n").map(|p| p.trim().to_owned()).filter(|p| !p.is_empty()).collect())
        .unwrap_or_default()
}

/// One event to one recipient. `content` Minimal names no CI (the caller passes `e.ci` None too).
pub fn single(locale: Locale, content: Content, custom: &Custom, e: &Event, why: &[Why], action: &str) -> Rendered {
    let (subject, body) = single_parts(locale, content, custom, e, why, action);
    layout(locale, subject, body)
}

/// The designer's test of an action (`POST .../actions/{key}/test`): the message [`single`] writes, marked as a
/// test in the subject and in a first paragraph that names `user`, who asked for it and is the only recipient.
pub fn action_test(locale: Locale, content: Content, custom: &Custom, e: &Event, action: &str, user: &str) -> Rendered {
    let (subject, mut body) = single_parts(locale, content, custom, e, &[Why::Test], action);
    body.intro.insert(0, t(locale, "intro.action_test", &[("action", &inline(action)), ("user", &inline(user))]));
    layout(locale, t(locale, "subject.action_test", &[("subject", &subject)]), body)
}

fn single_parts(
    locale: Locale,
    content: Content,
    custom: &Custom,
    e: &Event,
    why: &[Why],
    action: &str,
) -> (String, Body) {
    let args = event_args(locale, e);
    let a = targs(&args);
    let minimal = content == Content::Minimal || e.ci.is_none();
    let subject = match (&custom.subject, minimal) {
        (Some(s), true) => custom_text(locale, e, s),
        (Some(s), false) => {
            let reference = e.ci.as_ref().map(|c| inline(c.reference())).unwrap_or_default();
            format!("[{reference}] {}", custom_text(locale, e, s))
        }
        (None, true) => t(locale, "subject.minimal", &a),
        (None, false) => t(locale, &format!("subject.{}", e.kind.key()), &a),
    };
    let lead = if minimal { t(locale, "lead.minimal", &a) } else { t(locale, &format!("lead.{}", e.kind.key()), &a) };
    let body = Body {
        intro: intro(locale, e, custom),
        lead,
        details: detail_rows(locale, e, if minimal { Content::Minimal } else { content }),
        list_title: None,
        list: Vec::new(),
        link_text: t(locale, "open", &[]),
        link: e.url.clone(),
        footer: footer(locale, why, &e.workflow, action),
    };
    (subject, body)
}

/// One transition applied to many CIs by one bulk request: one message that
/// lists the CIs `visible` (those the recipient may view, by label, the
/// first [`BULK_LIST`]) and counts the rest of them. `first` is any of the
/// events, for the workflow, transition and actor.
pub fn bulk(
    locale: Locale,
    custom: &Custom,
    first: &Event,
    visible: &[Ci],
    list_url: &str,
    why: &[Why],
    action: &str,
) -> Rendered {
    let count = visible.len().to_string();
    let mut args = event_args(locale, first);
    args.push(("count", count.clone()));
    let a = targs(&args);
    let mut list: Vec<String> = visible
        .iter()
        .take(BULK_LIST)
        .map(|c| match &c.ident {
            Some(i) => format!("{} ({i})", c.label),
            None => c.label.clone(),
        })
        .collect();
    if visible.len() > BULK_LIST {
        list.push(t(locale, "more", &[("count", &(visible.len() - BULK_LIST).to_string())]));
    }
    // The CI placeholders name one CI; in a bulk message they are empty.
    let generic = Event { ci: None, ..first.clone() };
    let body = Body {
        intro: intro(locale, &generic, custom),
        lead: t(locale, "lead.bulk", &a),
        details: Vec::new(),
        list_title: None,
        list,
        link_text: t(locale, "open_list", &[]),
        link: list_url.to_owned(),
        footer: footer(locale, why, &first.workflow, action),
    };
    layout(locale, t(locale, "subject.bulk", &a), body)
}

/// The messages of one recipient past `MAIL_MAX_PER_RECIPIENT_PER_HOUR`, in
/// one: an entry per event (CIs by label only where the recipient may view
/// them; minimal entries name none).
pub fn digest(locale: Locale, events: &[Event], max: i32, list_url: &str) -> Rendered {
    let count = events.len().to_string();
    let mut list: Vec<String> = events
        .iter()
        .take(DIGEST_LIST)
        .map(|e| {
            let args = event_args(locale, e);
            let a = targs(&args);
            let what = if e.ci.is_none() {
                t(locale, "event.minimal", &a)
            } else {
                t(locale, &format!("event.{}", e.kind.key()), &a)
            };
            let ci = e.ci.as_ref().map(|c| inline(&c.label)).unwrap_or_else(|| "–".into());
            t(
                locale,
                "digest.entry",
                &[
                    ("time", &format_time(locale, e.at)),
                    ("workflow", &inline(&e.workflow)),
                    ("ci", &ci),
                    ("event", &what),
                ],
            )
        })
        .collect();
    if events.len() > DIGEST_LIST {
        list.push(t(locale, "more", &[("count", &(events.len() - DIGEST_LIST).to_string())]));
    }
    let body = Body {
        intro: Vec::new(),
        lead: t(locale, "lead.digest", &[("max", &max.to_string())]),
        details: Vec::new(),
        list_title: None,
        list,
        link_text: t(locale, "open_list", &[]),
        link: list_url.to_owned(),
        footer: vec![
            t(locale, "why.intro", &[("reasons", &t(locale, "why.workflows", &[]))]),
            t(locale, "footer.contact", &[]),
        ],
    };
    layout(locale, t(locale, "subject.digest", &[("count", &count)]), body)
}

/// The test message of `POST /admin/mail/test`.
pub fn test_message(locale: Locale, user: &str, public_url: &str) -> Rendered {
    let body = Body {
        intro: Vec::new(),
        lead: t(locale, "lead.test", &[("user", &inline(user))]),
        details: Vec::new(),
        list_title: None,
        list: Vec::new(),
        link_text: t(locale, "open", &[]),
        link: format!("{public_url}/"),
        footer: vec![
            t(locale, "why.intro", &[("reasons", &t(locale, "why.test", &[]))]),
            t(locale, "footer.contact", &[]),
        ],
    };
    layout(locale, t(locale, "subject.test", &[]), body)
}

// ---------------------------------------------------------------------------
// The message
// ---------------------------------------------------------------------------

/// `Auto-Submitted: auto-generated` (RFC 3834): out-of-office replies do not answer.
#[derive(Debug, Clone)]
struct AutoSubmitted;

impl Header for AutoSubmitted {
    fn name() -> HeaderName {
        HeaderName::new_from_ascii_str("Auto-Submitted")
    }
    fn parse(_: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(AutoSubmitted)
    }
    fn display(&self) -> HeaderValue {
        HeaderValue::new(Self::name(), "auto-generated".into())
    }
}

/// `X-Auto-Response-Suppress: All`: Exchange and Outlook send no automatic replies.
#[derive(Debug, Clone)]
struct AutoResponseSuppress;

impl Header for AutoResponseSuppress {
    fn name() -> HeaderName {
        HeaderName::new_from_ascii_str("X-Auto-Response-Suppress")
    }
    fn parse(_: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(AutoResponseSuppress)
    }
    fn display(&self) -> HeaderValue {
        HeaderValue::new(Self::name(), "All".into())
    }
}

/// The message of delivery `id` to `to`: multipart text and HTML, with the
/// delivery's stable `Message-ID`.
pub fn message(mail: &Mail, id: Uuid, to: Mailbox, r: &Rendered) -> Result<lettre::Message, lettre::error::Error> {
    let from = mail.from().cloned().unwrap_or_else(|| {
        Mailbox::new(Some("ShadouCMDB".into()), lettre::Address::new("shadoucmdb", "localhost").expect("valid address"))
    });
    let mut builder = lettre::Message::builder()
        .from(from)
        .to(to)
        .subject(subject_line(&r.subject))
        .message_id(Some(mail.message_id(id)))
        .header(AutoSubmitted)
        .header(AutoResponseSuppress);
    if let Some(reply_to) = mail.reply_to() {
        builder = builder.reply_to(reply_to.clone());
    }
    builder.multipart(
        MultiPart::alternative()
            .singlepart(SinglePart::builder().header(ContentType::TEXT_PLAIN).body(r.text.clone()))
            .singlepart(SinglePart::builder().header(ContentType::TEXT_HTML).body(r.html.clone())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(text: &str) -> Vec<(String, Vec<String>)> {
        let mut out: Vec<(String, Vec<String>)> = parse_catalogue(Box::leak(text.to_owned().into_boxed_str()))
            .into_iter()
            .map(|(k, v)| {
                let names = std::cell::RefCell::new(Vec::new());
                fill(v, "{", "}", &|n| {
                    names.borrow_mut().push(n.to_owned());
                    Some(String::new())
                });
                let mut names = names.into_inner();
                names.sort();
                (k.to_owned(), names)
            })
            .collect();
        out.sort();
        out
    }

    /// German has exactly the keys and placeholders of English.
    #[test]
    fn the_catalogues_match() {
        let en = keys(include_str!("templates/en.txt"));
        let de = keys(include_str!("templates/de.txt"));
        assert_eq!(en, de);
        assert!(en.len() > 60);
        // Conventions: no exclamation marks, the formal "Sie" in German.
        for (k, v) in EN.iter().chain(DE.iter()) {
            assert!(!v.contains('!'), "{k}: {v}");
        }
        for (k, v) in DE.iter() {
            assert!(!v.contains(" du ") && !v.contains(" dich ") && !v.contains(" dein"), "{k}: {v}");
        }
    }

    fn event() -> Event {
        Event {
            kind: EventKind::Transition,
            at: DateTime::parse_from_rfc3339("2026-10-09T08:00:00Z").unwrap().with_timezone(&Utc),
            workflow: "Change".into(),
            transition: Some("Approve".into()),
            from: Some("Planned".into()),
            to: Some("Approved".into()),
            actor: Some("j.doe".into()),
            comment: Some("ok".into()),
            ci: Some(Ci { label: "db01".into(), ident: Some("SRV-0042".into()), class: "Server".into() }),
            approval: None,
            fields: vec![("Owner".into(), "a CI you cannot view".into())],
            url: "https://cmdb.example/workflows/1".into(),
        }
    }

    #[test]
    fn subjects_follow_the_conventions_and_never_carry_a_line_break() {
        let e = event();
        let r = single(Locale::En, Content::Standard, &Custom::default(), &e, &[Why::User], "tell");
        assert_eq!(r.subject, "[SRV-0042] Change: db01 is now Approved");
        let r = single(Locale::De, Content::Standard, &Custom::default(), &e, &[Why::User], "tell");
        assert_eq!(r.subject, "[SRV-0042] Change: db01 ist jetzt Approved");
        assert!(r.text.contains("Sie erhalten diese Nachricht, weil die Aktion Sie namentlich nennt."), "{}", r.text);

        let evil = Event {
            ci: Some(Ci { label: "db01\r\nBcc: x@evil.example".into(), ident: None, class: "Server".into() }),
            ..event()
        };
        let custom = Custom { subject: Some("{{ci.label}}\nX-Injected: 1".into()), intro: None };
        for r in [
            single(Locale::En, Content::Standard, &Custom::default(), &evil, &[], "a"),
            single(Locale::En, Content::Standard, &custom, &evil, &[], "a"),
        ] {
            assert!(!r.subject.contains(['\r', '\n']), "{:?}", r.subject);
            assert!(r.subject.contains("db01 Bcc: x@evil.example"), "{:?}", r.subject);
        }
        let long = Custom { subject: Some("x".repeat(500)), intro: None };
        assert_eq!(single(Locale::En, Content::Standard, &long, &e, &[], "a").subject.chars().count(), MAX_SUBJECT);
    }

    #[test]
    fn minimal_content_names_no_ci_and_values_are_escaped() {
        let e = Event { ci: None, fields: Vec::new(), comment: None, ..event() };
        let custom =
            Custom { subject: Some("{{ci.label}} {{workflow.name}}".into()), intro: Some("{{ci.ident}}".into()) };
        let r = single(Locale::En, Content::Minimal, &custom, &e, &[Why::Address], "a");
        assert_eq!(r.subject, "Change");
        assert!(!r.text.contains("db01") && !r.html.contains("db01") && !r.text.contains("SRV"));
        assert!(r.text.contains("A workflow event needs your attention"));

        let xss = Event { comment: Some("<script>alert(1)</script>".into()), ..event() };
        let r = single(Locale::En, Content::Detailed, &Custom::default(), &xss, &[], "a");
        assert!(!r.html.contains("<script>") && r.html.contains("&lt;script&gt;"));
        assert!(r.text.contains("Owner:") && r.text.contains("a CI you cannot view"), "{}", r.text);
        // A value that looks like a layout slot is not expanded.
        let slot = Event { comment: Some("{{link}} {{footer}}".into()), ..event() };
        let r = single(Locale::En, Content::Standard, &Custom::default(), &slot, &[], "a");
        assert!(r.html.contains("{{link}} {{footer}}"));
    }

    #[test]
    fn a_bulk_message_lists_fifty_and_counts_the_rest() {
        let cis: Vec<Ci> =
            (0..2000).map(|i| Ci { label: format!("srv{i:04}"), ident: None, class: "Server".into() }).collect();
        let r = bulk(Locale::En, &Custom::default(), &event(), &cis, "https://cmdb.example/workflows", &[], "a");
        assert_eq!(r.subject, "Change: Approve was applied to 2000 configuration items");
        assert!(r.text.contains("- srv0049\n") && !r.text.contains("srv0050"));
        assert!(r.text.contains("and 1950 more"));
    }
}
