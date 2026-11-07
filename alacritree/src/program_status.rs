//! OSC 7501, the Program Status Protocol: records a program keeps in the
//! terminal, keyed by `id`, saying what it is doing.
//!
//! Every report is untrusted input, so one that breaks a rule of the spec
//! changes nothing. The spec is
//! <https://superlogical.com/rex/docs/build/program-status>.

use std::collections::HashMap;

use alacritree_common::agent_status::AgentStatus;
use base64::Engine;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

/// What a blocked program waits for. Each variant is spelled as the protocol
/// spells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::EnumString)]
#[strum(serialize_all = "lowercase")]
pub(crate) enum BlockedKind {
    /// Approval to do something.
    Permission,
    /// An answer the user must type.
    Question,
    /// A login, token or credential.
    Auth,
}

/// What one record says. `state` is the one key every report carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Status {
    pub state: AgentStatus,
    /// Only a `blocked` record has one.
    pub kind: Option<BlockedKind>,
    /// The program's stable identifier, such as `claude-code`.
    pub app: Option<String>,
    /// A short label for the record.
    pub title: Option<String>,
    /// One line on what the record is doing, waiting for, or finished.
    pub msg: Option<String>,
}

impl Status {
    #[cfg(test)]
    pub(crate) fn new(state: AgentStatus) -> Self {
        Self { state, kind: None, app: None, title: None, msg: None }
    }

    /// One line for a notification. The program's own `msg` wins, and
    /// without one the line says what the state and kind mean.
    pub(crate) fn summary(&self) -> String {
        if let Some(msg) = &self.msg {
            return msg.clone();
        }
        let who = self.app.as_deref().map_or_else(|| "The program".to_owned(), app_name);
        match (self.state, self.kind) {
            (AgentStatus::Blocked, Some(BlockedKind::Permission)) => {
                format!("{who} needs permission")
            },
            (AgentStatus::Blocked, Some(BlockedKind::Question)) => {
                format!("{who} has a question")
            },
            (AgentStatus::Blocked, Some(BlockedKind::Auth)) => format!("{who} needs a login"),
            (AgentStatus::Blocked, None) => format!("{who} is waiting for you"),
            (AgentStatus::Done, _) => format!("{who} finished"),
            (AgentStatus::Error, _) => format!("{who} failed"),
            (AgentStatus::Working, _) => format!("{who} is working"),
            (AgentStatus::Idle | AgentStatus::Unknown, _) => format!("{who} is idle"),
        }
    }
}

/// An `app` or agent identifier as a person reads it: `claude-code` is
/// `Claude Code`.
pub(crate) fn app_name(id: &str) -> String {
    let words = id.split(['-', '_']).filter(|word| !word.is_empty()).map(|word| {
        let mut chars = word.chars();
        chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
    });
    words.collect::<Vec<_>>().join(" ")
}

/// A report that changes the records. `None` addresses the root record for
/// a status, and every record for `Clear`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Report {
    Set { id: Option<String>, status: Status },
    Clear { id: Option<String> },
}

/// The whole sequence, `OSC` through `ST`.
const MAX_SEQUENCE: usize = 4096;
/// The longest spelling of everything around the body.
const FRAMING: usize = "\x1b]7501;".len() + "\x1b\\".len();
/// `vte` keeps this many `;`-separated params and drops the rest.
const VTE_MAX_PARAMS: usize = 16;
const MAX_KEY: usize = 16;
const MAX_ID: usize = 128;
const MAX_ID_SEGMENT: usize = 32;
const MAX_ID_DEPTH: usize = 8;
const MAX_APP: usize = 32;
/// Encoded and decoded byte limits of the two free-text keys.
const MSG_LIMITS: (usize, usize) = (2732, 2048);
const TITLE_LIMITS: (usize, usize) = (256, 192);
/// The spec's maximum. It requires at least 64.
pub(crate) const MAX_RECORDS: usize = 256;

/// Whether `params` is the feature query `OSC 7501 ; ?`.
pub(crate) fn is_query(params: &[&[u8]]) -> bool {
    params == [b"7501".as_slice(), b"?"]
}

/// The reply to the feature query. A bare `?` means the default states and
/// kinds. It ends the way the query did, as the terminal's other OSC replies
/// do.
pub(crate) fn query_reply(bell_terminated: bool) -> String {
    let terminator = if bell_terminated { "\x07" } else { "\x1b\\" };
    format!("\x1b]7501;?{terminator}")
}

/// The report `params` carries, or `None` when the spec says to ignore it.
/// `params` is the sequence as `vte` split it on `;`.
pub(crate) fn parse(params: &[&[u8]]) -> Option<Report> {
    // At the cap, `vte` may have dropped the end of the body.
    if params.first().copied() != Some(b"7501") || params.len() >= VTE_MAX_PARAMS {
        return None;
    }
    // A `;` is outside the value alphabet, so a body `vte` split is put back
    // together and its pairs fail validation one by one.
    let body = params[1..].join(&b';');
    if body.len() + FRAMING > MAX_SEQUENCE {
        return None;
    }
    let body = std::str::from_utf8(&body).ok()?;

    let mut fields: HashMap<&str, &str> = HashMap::new();
    for pair in body.split(':') {
        let Some((key, value)) = pair.split_once('=') else { continue };
        if !is_key(key) {
            continue;
        }
        if value.bytes().all(is_value_byte) {
            fields.insert(key, value);
        } else if key == "id" {
            // Skipping it would address the root record instead.
            return None;
        }
    }

    let id = match fields.get("id") {
        Some(id) => Some(valid_id(id)?.to_owned()),
        None => None,
    };
    let app = fields.get("app").map(|app| (*app).to_owned());
    if app.as_ref().is_some_and(|app| !is_name(app, MAX_APP)) {
        return None;
    }
    let text = |key, limits| match fields.get(key) {
        Some(encoded) => decode_text(encoded, limits).map(Some),
        None => Some(None),
    };
    let msg = text("msg", MSG_LIMITS)?;
    let title = text("title", TITLE_LIMITS)?;

    let state: AgentStatus = match *fields.get("state")? {
        "clear" => return Some(Report::Clear { id }),
        state => state.parse().ok()?,
    };
    // An unknown kind, or one on a state other than `blocked`, is absent.
    let kind = fields
        .get("kind")
        .filter(|_| state == AgentStatus::Blocked)
        .and_then(|kind| kind.parse().ok());
    Some(Report::Set { id, status: Status { state, kind, app, title, msg } })
}

fn is_key(key: &str) -> bool {
    (1..=MAX_KEY).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_lowercase())
}

fn is_value_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"_.,+/=-".contains(&byte)
}

/// An `app`, or one segment of an `id`.
fn is_name(name: &str, max: usize) -> bool {
    (1..=max).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
}

fn valid_id(id: &str) -> Option<&str> {
    let ok = id.len() <= MAX_ID
        && id.split('/').count() <= MAX_ID_DEPTH
        && id.split('/').all(|segment| is_name(segment, MAX_ID_SEGMENT));
    ok.then_some(id)
}

/// The spec's base64 accepts the padding or leaves it off.
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// A free-text value's text, or `None` when it breaks a rule.
fn decode_text(encoded: &str, (max_encoded, max_decoded): (usize, usize)) -> Option<String> {
    if encoded.len() > max_encoded {
        return None;
    }
    let decoded = BASE64.decode(encoded).ok()?;
    if decoded.len() > max_decoded {
        return None;
    }
    let text = String::from_utf8(decoded).ok()?;
    (!text.chars().any(char::is_control)).then_some(text)
}

struct Record {
    status: Status,
    /// When the record was last written, on the store's own clock.
    updated: u64,
}

/// One terminal's records. The root record has the empty id.
#[derive(Default)]
pub(crate) struct Records {
    records: HashMap<String, Record>,
    clock: u64,
}

impl Records {
    pub(crate) fn apply(&mut self, report: Report) {
        match report {
            Report::Set { id, status } => self.set(id.unwrap_or_default(), status),
            Report::Clear { id: None } => self.records.clear(),
            Report::Clear { id: Some(id) } => {
                let prefix = format!("{id}/");
                self.records.retain(|key, _| *key != id && !key.starts_with(&prefix));
            },
        }
    }

    fn set(&mut self, id: String, status: Status) {
        self.clock += 1;
        if self.records.len() >= MAX_RECORDS && !self.records.contains_key(&id) {
            let oldest = self.records.iter().min_by_key(|(_, r)| r.updated).map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                self.records.remove(&oldest);
            }
        }
        self.records.insert(id, Record { status, updated: self.clock });
    }

    /// The program exited or a new prompt began, so nothing it reported is
    /// still in flight. A result stays.
    pub(crate) fn end_activity(&mut self) {
        self.records.retain(|_, record| {
            !matches!(record.status.state, AgentStatus::Working | AgentStatus::Blocked)
        });
    }

    /// A full reset (RIS).
    pub(crate) fn clear(&mut self) {
        self.records.clear();
    }

    /// The record that most wants a human, the newest of those that tie, or
    /// `None` while the program has reported nothing. A record without an
    /// `app` takes its nearest ancestor's.
    pub(crate) fn shown(&self) -> Option<Status> {
        let (id, record) =
            self.records.iter().max_by_key(|(_, record)| (record.status.state, record.updated))?;
        let mut status = record.status.clone();
        if status.app.is_none() {
            status.app = self.ancestors(id).find_map(|record| record.status.app.clone());
        }
        Some(status)
    }

    /// The records above `id`, nearest first, ending at the root.
    fn ancestors<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a Record> {
        let parents = std::iter::successors(Some(id), |id| {
            (!id.is_empty()).then(|| id.rsplit_once('/').map_or("", |(parent, _)| parent))
        });
        parents.skip(1).filter_map(|parent| self.records.get(parent))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(body: &str) -> Option<Report> {
        let params: Vec<&[u8]> =
            std::iter::once(b"7501".as_slice()).chain(body.split(';').map(str::as_bytes)).collect();
        parse(&params)
    }

    fn set(id: Option<&str>, state: AgentStatus) -> Option<Report> {
        Some(Report::Set { id: id.map(str::to_owned), status: Status::new(state) })
    }

    fn shown_state(records: &Records) -> Option<AgentStatus> {
        records.shown().map(|status| status.state)
    }

    fn records(bodies: &[&str]) -> Records {
        let mut records = Records::default();
        for body in bodies {
            records.apply(report(body).unwrap_or_else(|| panic!("{body:?} was ignored")));
        }
        records
    }

    #[test]
    fn each_state_parses_with_and_without_an_id() {
        use AgentStatus::*;
        for (word, state) in [
            ("idle", Idle),
            ("working", Working),
            ("done", Done),
            ("error", Error),
            ("blocked", Blocked),
        ] {
            assert_eq!(report(&format!("state={word}")), set(None, state));
            assert_eq!(
                report(&format!("state={word}:id=build/test")),
                set(Some("build/test"), state)
            );
        }
        assert_eq!(report("state=clear:id=a"), Some(Report::Clear { id: Some("a".into()) }));
        assert_eq!(report("state=clear"), Some(Report::Clear { id: None }));
    }

    /// The spec's own examples.
    #[test]
    fn the_spec_examples_parse() {
        assert_eq!(
            report("state=working:app=brew:msg=SW5zdGFsbGluZyB1cGRhdGVz"),
            Some(Report::Set {
                id: None,
                status: Status {
                    app: Some("brew".into()),
                    msg: Some("Installing updates".into()),
                    ..Status::new(AgentStatus::Working)
                },
            })
        );
        assert_eq!(
            report(
                "state=blocked:kind=permission:app=terraform:\
                 msg=QXBwbHkgMyB0byBhZGQsIDEgdG8gY2hhbmdlLCAwIHRvIGRlc3Ryb3k/:title=cGxhbg"
            ),
            Some(Report::Set {
                id: None,
                status: Status {
                    kind: Some(BlockedKind::Permission),
                    app: Some("terraform".into()),
                    title: Some("plan".into()),
                    msg: Some("Apply 3 to add, 1 to change, 0 to destroy?".into()),
                    ..Status::new(AgentStatus::Blocked)
                },
            })
        );
    }

    /// A kind is absent when the protocol does not know it or the state is
    /// not `blocked`.
    #[test]
    fn a_kind_counts_only_when_known_and_blocked() {
        assert_eq!(report("state=working:kind=question"), set(None, AgentStatus::Working));
        assert_eq!(report("state=blocked:kind=coffee"), set(None, AgentStatus::Blocked));
        for (word, kind) in [
            ("permission", BlockedKind::Permission),
            ("question", BlockedKind::Question),
            ("auth", BlockedKind::Auth),
        ] {
            let Some(Report::Set { status, .. }) = report(&format!("state=blocked:kind={word}"))
            else {
                panic!("{word} was ignored")
            };
            assert_eq!(status.kind, Some(kind));
        }
    }

    #[test]
    fn malformed_pairs_and_unknown_keys_are_skipped_and_the_last_value_wins() {
        assert_eq!(
            report("garbage:state=idle:Bad=x:unknown=1:v=a;b:state=working"),
            set(None, AgentStatus::Working)
        );
    }

    #[test]
    fn a_report_without_a_recognised_state_is_ignored() {
        for body in ["", "id=a", "state=", "state=sleeping", "state=Working", "state"] {
            assert_eq!(report(body), None, "{body:?}");
        }
    }

    #[test]
    fn a_report_with_a_malformed_id_is_ignored() {
        let long_segment = "a".repeat(33);
        let deep = ["a"; 9].join("/");
        let long = ["abcdefghijklmnop"; 8].join("/") + "x";
        for id in ["", "a//b", "/a", "a/", "a b", "a*b", &long_segment, &deep, &long] {
            assert_eq!(report(&format!("state=idle:id={id}")), None, "{id:?}");
        }
        let deepest = ["a"; 8].join("/");
        assert_eq!(
            report(&format!("state=idle:id={deepest}")),
            set(Some(&deepest), AgentStatus::Idle)
        );
    }

    #[test]
    fn a_report_with_invalid_or_unsafe_text_is_ignored() {
        let encode = |text: &str| base64::engine::general_purpose::STANDARD.encode(text);
        for msg in [
            "A".to_string(),
            "QQ=A".to_string(),
            encode("line\nbreak"),
            encode("\u{1b}[31m"),
            base64::engine::general_purpose::STANDARD.encode([0xff, 0xfe]),
        ] {
            assert_eq!(report(&format!("state=idle:msg={msg}")), None, "{msg:?}");
        }
        assert!(report(&format!("state=idle:msg={}", encode("ok"))).is_some());
        assert!(report("state=idle:msg=b2s").is_some(), "padding is optional");
    }

    #[test]
    fn a_report_over_a_size_limit_is_ignored() {
        let encode = |n: usize| base64::engine::general_purpose::STANDARD.encode("a".repeat(n));
        assert!(report(&format!("state=idle:msg={}", encode(2048))).is_some());
        assert_eq!(report(&format!("state=idle:msg={}", encode(2049))), None);
        assert!(report(&format!("state=idle:title={}", encode(192))).is_some());
        assert_eq!(report(&format!("state=idle:title={}", encode(193))), None);
        assert!(report(&format!("state=idle:app={}", "a".repeat(32))).is_some());
        assert_eq!(report(&format!("state=idle:app={}", "a".repeat(33))), None);
        for app in ["", "a/b", "a,b", "a=b"] {
            assert_eq!(report(&format!("state=idle:app={app}")), None, "{app:?}");
        }
        // `ESC ] 7501 ;` and `ESC \` take nine of the sequence's 4096 bytes.
        let body = |len: usize| format!("state=idle:pad={}", "x".repeat(len - 15));
        assert!(report(&body(4087)).is_some());
        assert_eq!(report(&body(4088)), None);
    }

    /// `vte` drops whatever follows its sixteenth param, so a report that
    /// reaches the cap may be missing its end.
    #[test]
    fn a_report_vte_may_have_cut_short_is_ignored() {
        // `7501` plus the body's pieces: fifteen params, then sixteen.
        assert!(report(&format!("{}:state=idle", "x;".repeat(13))).is_some());
        assert_eq!(report(&format!("{}:state=idle", "x;".repeat(14))), None);
    }

    #[test]
    fn each_report_replaces_its_record() {
        let records = records(&["state=blocked:id=a", "state=idle:id=a"]);
        assert_eq!(records.records.len(), 1);
        assert_eq!(shown_state(&records), Some(AgentStatus::Idle));
    }

    #[test]
    fn clear_removes_the_record_and_its_subtree() {
        let records = records(&[
            "state=blocked:id=a",
            "state=blocked:id=a/b",
            "state=blocked:id=a/b/c",
            "state=working:id=ab",
            "state=clear:id=a",
        ]);
        assert_eq!(records.records.len(), 1);
        assert_eq!(shown_state(&records), Some(AgentStatus::Working));
    }

    #[test]
    fn clear_without_an_id_removes_every_record() {
        let records = records(&["state=blocked", "state=working:id=a/b", "state=clear"]);
        assert_eq!(shown_state(&records), None);
    }

    #[test]
    fn the_record_that_most_wants_a_human_is_shown() {
        use AgentStatus::*;
        let shown = |bodies: &[&str]| shown_state(&records(bodies));
        assert_eq!(shown(&["state=working:id=a", "state=blocked:id=b"]), Some(Blocked));
        assert_eq!(shown(&["state=blocked:id=b", "state=working:id=a"]), Some(Blocked));
        assert_eq!(shown(&["state=done:id=a", "state=error:id=b"]), Some(Error));
        assert_eq!(shown(&["state=working:id=a", "state=done:id=b"]), Some(Done));
        assert_eq!(shown(&["state=idle:id=a", "state=working:id=b"]), Some(Working));
    }

    /// Among records that want a human equally, the newest one shows, with
    /// the `app` its nearest ancestor names when it names none.
    #[test]
    fn the_newest_of_tied_records_shows_with_an_inherited_app() {
        let records = records(&[
            "state=working:app=claude-code",
            "state=blocked:id=a:app=other",
            "state=blocked:id=b:msg=c2Vjb25k",
            "state=blocked:id=b/c:msg=dGhpcmQ",
        ]);
        let status = records.shown().unwrap();
        assert_eq!(status.msg.as_deref(), Some("third"));
        assert_eq!(status.app.as_deref(), Some("claude-code"));
    }

    #[test]
    fn a_summary_is_the_programs_msg_or_what_its_state_means() {
        use AgentStatus::*;
        use BlockedKind::*;
        let status = |state, kind, msg: Option<&str>| Status {
            kind,
            app: Some("claude-code".into()),
            msg: msg.map(str::to_owned),
            ..Status::new(state)
        };
        for (status, line) in [
            (status(Blocked, Some(Question), Some("Which parser?")), "Which parser?"),
            (status(Blocked, Some(Question), None), "Claude Code has a question"),
            (status(Blocked, Some(Permission), None), "Claude Code needs permission"),
            (status(Blocked, Some(Auth), None), "Claude Code needs a login"),
            (status(Blocked, None, None), "Claude Code is waiting for you"),
            (status(Done, None, None), "Claude Code finished"),
            (status(Error, None, None), "Claude Code failed"),
        ] {
            assert_eq!(status.summary(), line);
        }
        assert_eq!(Status::new(Done).summary(), "The program finished");
    }

    #[test]
    fn an_app_id_reads_as_words() {
        for (id, name) in [
            ("claude-code", "Claude Code"),
            ("claude", "Claude"),
            ("my_tool.v2", "My Tool.v2"),
            ("a--b", "A B"),
        ] {
            assert_eq!(app_name(id), name);
        }
    }

    #[test]
    fn a_full_store_drops_the_least_recently_updated_record() {
        let mut records = Records::default();
        for n in 0..MAX_RECORDS {
            let state = if n == 0 { "blocked" } else { "idle" };
            records.apply(report(&format!("state={state}:id=r{n}")).unwrap());
        }
        // Rewriting the first record makes the second the oldest.
        records.apply(report("state=blocked:id=r0").unwrap());
        records.apply(report("state=working:id=new").unwrap());
        assert_eq!(records.records.len(), MAX_RECORDS);
        assert!(records.records.contains_key("r0"));
        assert!(!records.records.contains_key("r1"));
        assert_eq!(shown_state(&records), Some(AgentStatus::Blocked));
    }

    #[test]
    fn ending_activity_drops_working_and_blocked_and_keeps_results() {
        let mut records = records(&[
            "state=working:id=a",
            "state=blocked:id=b",
            "state=done:id=c",
            "state=error:id=d",
        ]);
        records.end_activity();
        assert_eq!(records.records.len(), 2);
        assert_eq!(shown_state(&records), Some(AgentStatus::Error));
    }

    #[test]
    fn the_query_is_recognised_and_answered_with_its_own_terminator() {
        assert!(is_query(&[b"7501", b"?"]));
        assert!(!is_query(&[b"7501", b"?", b""]));
        assert!(!is_query(&[b"7501", b"state=idle"]));
        assert_eq!(query_reply(false), "\x1b]7501;?\x1b\\");
        assert_eq!(query_reply(true), "\x1b]7501;?\x07");
        assert_eq!(report("?"), None);
    }
}
