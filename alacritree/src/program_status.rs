//! OSC 7501, the Program Status Protocol: records a program keeps in the
//! terminal, keyed by `id`, saying what it is doing.
//!
//! Every report is untrusted input, so one that breaks a rule of the spec
//! changes nothing. The spec is
//! <https://superlogical.com/rex/docs/build/program-status>.

use std::collections::HashMap;

use base64::Engine;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

/// What a record says, ordered by how much it wants a human, which is how a
/// session with several records picks the one its row shows. Each variant is
/// spelled as the protocol spells it.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, strum::EnumString, strum::IntoStaticStr,
)]
#[strum(serialize_all = "lowercase")]
pub(crate) enum ProgramState {
    Idle,
    Working,
    Done,
    /// The program failed. It ranks above `done`, since a failure is the
    /// result to look at first.
    Error,
    Blocked,
}

impl ProgramState {
    /// Whether the program has finished and left a result to look at.
    pub(crate) fn is_result(self) -> bool {
        matches!(self, Self::Done | Self::Error)
    }

    /// Whether the record outlives the program that wrote it.
    fn survives_exit(self) -> bool {
        self == Self::Idle || self.is_result()
    }
}

/// A report that changes the records. `None` addresses the root record for
/// a state, and every record for `Clear`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Report {
    Set { id: Option<String>, state: ProgramState },
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
    if fields.get("app").is_some_and(|app| app.len() > MAX_APP) {
        return None;
    }
    for (key, limits) in [("msg", MSG_LIMITS), ("title", TITLE_LIMITS)] {
        if let Some(text) = fields.get(key) {
            valid_text(text, limits)?;
        }
    }

    match *fields.get("state")? {
        "clear" => Some(Report::Clear { id }),
        state => Some(Report::Set { id, state: state.parse().ok()? }),
    }
}

fn is_key(key: &str) -> bool {
    (1..=MAX_KEY).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_lowercase())
}

fn is_value_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"_.,+/=-".contains(&byte)
}

fn valid_id(id: &str) -> Option<&str> {
    let segment_ok = |segment: &str| {
        (1..=MAX_ID_SEGMENT).contains(&segment.len())
            && segment.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
    };
    let ok = id.len() <= MAX_ID
        && id.split('/').count() <= MAX_ID_DEPTH
        && id.split('/').all(segment_ok);
    ok.then_some(id)
}

/// The spec's base64 accepts the padding or leaves it off.
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// Checks a free-text value. The text itself is not kept, since nothing
/// shows it.
fn valid_text(encoded: &str, (max_encoded, max_decoded): (usize, usize)) -> Option<()> {
    if encoded.len() > max_encoded {
        return None;
    }
    let decoded = BASE64.decode(encoded).ok()?;
    if decoded.len() > max_decoded {
        return None;
    }
    let text = String::from_utf8(decoded).ok()?;
    (!text.chars().any(char::is_control)).then_some(())
}

struct Record {
    state: ProgramState,
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
            Report::Set { id, state } => self.set(id.unwrap_or_default(), state),
            Report::Clear { id: None } => self.records.clear(),
            Report::Clear { id: Some(id) } => {
                let prefix = format!("{id}/");
                self.records.retain(|key, _| *key != id && !key.starts_with(&prefix));
            },
        }
    }

    fn set(&mut self, id: String, state: ProgramState) {
        self.clock += 1;
        if self.records.len() >= MAX_RECORDS && !self.records.contains_key(&id) {
            let oldest = self.records.iter().min_by_key(|(_, r)| r.updated).map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                self.records.remove(&oldest);
            }
        }
        self.records.insert(id, Record { state, updated: self.clock });
    }

    /// The program exited or a new prompt began, so nothing it reported is
    /// still in flight. A result stays.
    pub(crate) fn end_activity(&mut self) {
        self.records.retain(|_, record| record.state.survives_exit());
    }

    /// A full reset (RIS).
    pub(crate) fn clear(&mut self) {
        self.records.clear();
    }

    /// The state of the record that most wants a human, or `None` while the
    /// program has reported nothing.
    pub(crate) fn shown(&self) -> Option<ProgramState> {
        self.records.values().map(|record| record.state).max()
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

    fn set(id: Option<&str>, state: ProgramState) -> Option<Report> {
        Some(Report::Set { id: id.map(str::to_owned), state })
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
        use ProgramState::*;
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

    /// The spec's own examples, `msg` included.
    #[test]
    fn the_spec_examples_parse() {
        assert_eq!(
            report("state=working:app=brew:msg=SW5zdGFsbGluZyB1cGRhdGVz"),
            set(None, ProgramState::Working)
        );
        assert_eq!(
            report(
                "state=blocked:kind=permission:app=terraform:\
                 msg=QXBwbHkgMyB0byBhZGQsIDEgdG8gY2hhbmdlLCAwIHRvIGRlc3Ryb3k/"
            ),
            set(None, ProgramState::Blocked)
        );
    }

    #[test]
    fn malformed_pairs_and_unknown_keys_are_skipped_and_the_last_value_wins() {
        assert_eq!(
            report("garbage:state=idle:Bad=x:unknown=1:v=a;b:state=working"),
            set(None, ProgramState::Working)
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
            set(Some(&deepest), ProgramState::Idle)
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
        assert_eq!(records.shown(), Some(ProgramState::Idle));
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
        assert_eq!(records.shown(), Some(ProgramState::Working));
    }

    #[test]
    fn clear_without_an_id_removes_every_record() {
        let records = records(&["state=blocked", "state=working:id=a/b", "state=clear"]);
        assert_eq!(records.shown(), None);
    }

    #[test]
    fn the_record_that_most_wants_a_human_is_shown() {
        use ProgramState::*;
        let shown = |bodies: &[&str]| records(bodies).shown();
        assert_eq!(shown(&["state=working:id=a", "state=blocked:id=b"]), Some(Blocked));
        assert_eq!(shown(&["state=blocked:id=b", "state=working:id=a"]), Some(Blocked));
        assert_eq!(shown(&["state=done:id=a", "state=error:id=b"]), Some(Error));
        assert_eq!(shown(&["state=working:id=a", "state=done:id=b"]), Some(Done));
        assert_eq!(shown(&["state=idle:id=a", "state=working:id=b"]), Some(Working));
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
        assert_eq!(records.shown(), Some(ProgramState::Blocked));
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
        assert_eq!(records.shown(), Some(ProgramState::Error));
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
