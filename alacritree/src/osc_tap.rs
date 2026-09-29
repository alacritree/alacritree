//! The OSC sequences `vte`'s `ansi` layer recognises and drops, read off a
//! copy of the PTY byte stream.
//!
//! The tap thread feeds the pure decisions below, while the session that
//! consumes its output lives elsewhere. Every rule is testable without a PTY.

use std::sync::OnceLock;

use alacritty_terminal::vte;

use crate::config::VtConfig;
use crate::repaint::Repaint;

/// What the shell on the other end of the PTY spells a path like.  A WSL
/// session on Windows is `Unix`: the payload is a Linux path, and the
/// translation to a Windows one happens later, against the session's distro.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellPlatform {
    Unix,
    Windows,
}

/// The ConEmu progress states, already bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Progress {
    Clear,
    Set(u8),
    Error(u8),
    Indeterminate,
    Paused(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OscEvent {
    /// `None` clears the reported directory.  The path is still the shell's
    /// own spelling: WSL translation needs the session's distro.
    Cwd(Option<String>),
    Notify(String),
    Progress(Progress),
    PointerShape(egui::CursorIcon),
}

pub(crate) struct TapPolicy {
    pub(crate) vt: VtConfig,
    pub(crate) hostname: String,
    pub(crate) shell: ShellPlatform,
}

/// This machine's name, resolved once.  An empty string when the lookup
/// fails, which leaves only an empty host and `localhost` counting as local.
pub(crate) fn local_hostname() -> &'static str {
    static HOSTNAME: OnceLock<String> = OnceLock::new();
    HOSTNAME.get_or_init(|| gethostname::gethostname().to_string_lossy().into_owned())
}

pub(crate) fn classify(params: &[&[u8]], policy: &TapPolicy) -> Option<OscEvent> {
    let event = classify_inner(params, policy);
    if event.is_none() {
        log::debug!("discarding OSC sequence with params: {params:?}");
    }
    event
}

fn classify_inner(params: &[&[u8]], policy: &TapPolicy) -> Option<OscEvent> {
    match params.first().copied()? {
        b"7" => {
            if !policy.vt.report_cwd {
                return None;
            }
            let payload = rejoin(params, 1)?;
            if payload.is_empty() {
                return Some(OscEvent::Cwd(None));
            }
            let path = file_url_path(&payload, &policy.hostname)?;
            Some(OscEvent::Cwd(Some(rooted(&path, policy.shell)?)))
        },
        b"9" => classify_osc9(params, policy),
        b"777" => {
            if !policy.vt.notify {
                return None;
            }
            // `777;notify;<title>;<body>`; the body may hold semicolons.
            if params.get(1).copied()? != b"notify" {
                return None;
            }
            let body = rejoin(params, 3).filter(|b| !b.is_empty())?;
            Some(OscEvent::Notify(body))
        },
        b"22" => {
            if !policy.vt.pointer_shape {
                return None;
            }
            cursor_icon(&rejoin(params, 1)?).map(OscEvent::PointerShape)
        },
        _ => None,
    }
}

/// OSC 9 carries two unrelated protocols. A digit-run first field selects
/// ConEmu, so `9;9 items remaining` stays a notification.
fn classify_osc9(params: &[&[u8]], policy: &TapPolicy) -> Option<OscEvent> {
    let subcommand = params
        .get(1)
        .copied()
        .filter(|field| !field.is_empty() && field.iter().all(|byte| byte.is_ascii_digit()));
    match subcommand {
        Some(b"4") => {
            if params.len() < 3 {
                return None;
            }
            if !policy.vt.progress {
                return None;
            }
            progress(params).map(OscEvent::Progress)
        },
        Some(b"9") => {
            if !policy.vt.report_cwd {
                return None;
            }
            let raw = rejoin(params, 2)?;
            let path = unquote(&raw);
            Some(OscEvent::Cwd(Some(rooted(path, policy.shell)?)))
        },
        Some(_) => None,
        None => {
            if !policy.vt.notify {
                return None;
            }
            let body = rejoin(params, 1).filter(|b| !b.is_empty())?;
            Some(OscEvent::Notify(body))
        },
    }
}

/// Windows Terminal's `DoConEmuAction` sets these bounds: a state above the
/// highest defined one rejects the whole sequence, progress above a hundred
/// clamps, and an absent or empty state means zero.
fn progress(params: &[&[u8]]) -> Option<Progress> {
    let parse_field = |raw: &[u8]| -> Option<u8> {
        if raw.is_empty() {
            return Some(0);
        }
        std::str::from_utf8(raw).ok()?.parse::<u32>().ok().map(|n| n.min(100) as u8)
    };
    let state = params.get(2).map(|raw| parse_field(raw)).unwrap_or(Some(0))?;
    let value = params.get(3).map(|raw| parse_field(raw)).unwrap_or(Some(0))?;
    match state {
        0 => Some(Progress::Clear),
        1 => Some(Progress::Set(value)),
        2 => Some(Progress::Error(value)),
        3 => Some(Progress::Indeterminate),
        4 => Some(Progress::Paused(value)),
        _ => None,
    }
}

/// vte splits OSC parameters on semicolons, so any payload that may legally
/// contain one is put back together.
fn rejoin(params: &[&[u8]], from: usize) -> Option<String> {
    let parts = params.get(from..)?;
    let joined = parts.iter().map(|p| String::from_utf8_lossy(p)).collect::<Vec<_>>().join(";");
    Some(joined)
}

/// ConEmu documents the 9;9 payload as a quoted string.  Windows Terminal
/// strips the quotes when both are present and parses the bare string when
/// they are not, and says in a comment that ConEmu does the same.
fn unquote(raw: &str) -> &str {
    raw.strip_prefix('"').and_then(|r| r.strip_suffix('"')).unwrap_or(raw)
}

/// The host check filters shells that honestly name a remote host, which is
/// the common accident.  It stops nobody writing bytes with intent, who
/// simply writes `localhost`, so nothing downstream may lean on it.
fn file_url_path(url: &str, hostname: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let local = host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || (!hostname.is_empty() && host.eq_ignore_ascii_case(hostname));
    if !local {
        return None;
    }
    let decoded = percent_decode(path)?;
    // `/C:/src` is how a Windows path travels in a file URL.
    let trimmed = match decoded.strip_prefix('/') {
        Some(r) if is_drive_rooted(r) => r.to_string(),
        _ => decoded,
    };
    Some(trimmed)
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let hi = (hex[0] as char).to_digit(16)?;
            let lo = (hex[1] as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn is_drive_rooted(path: &str) -> bool {
    let mut chars = path.chars();
    matches!(
        (chars.next(), chars.next(), chars.next()),
        (Some(c), Some(':'), Some('/' | '\\')) if c.is_ascii_alphabetic()
    )
}

/// The guard the rest of the design rests on.  A path beginning `\\` or `//`
/// is UNC on Windows: touching it with `is_dir` opens an SMB connection to a
/// host the payload named, and the payload need not come from a remote shell
/// at all, since `cat` of a downloaded file reaches the PTY the same way.
/// Anything not rooted for the shell's own platform is dropped with it.
fn rooted(path: &str, shell: ShellPlatform) -> Option<String> {
    if path.starts_with("\\\\") || path.starts_with("//") {
        return None;
    }
    let ok = match shell {
        ShellPlatform::Unix => path.starts_with('/'),
        ShellPlatform::Windows => is_drive_rooted(path),
    };
    ok.then(|| path.to_string())
}

/// The xterm pointer names alacritree has a cursor for.  An unknown name
/// leaves the current shape alone: not understanding a request is not a
/// request to reset.
fn cursor_icon(name: &str) -> Option<egui::CursorIcon> {
    Some(match name {
        "default" | "left_ptr" | "arrow" => egui::CursorIcon::Default,
        "pointer" | "hand" | "hand2" => egui::CursorIcon::PointingHand,
        "text" | "xterm" | "ibeam" => egui::CursorIcon::Text,
        "crosshair" | "cross" => egui::CursorIcon::Crosshair,
        "wait" | "watch" => egui::CursorIcon::Wait,
        "progress" => egui::CursorIcon::Progress,
        "help" | "question_arrow" => egui::CursorIcon::Help,
        "move" | "fleur" => egui::CursorIcon::Move,
        "not-allowed" | "crossed_circle" => egui::CursorIcon::NotAllowed,
        _ => return None,
    })
}

/// How many bytes one sequence may occupy before the parser is reset.
pub(crate) const MAX_PENDING: usize = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanState {
    Ground,
    Escape,
    Osc,
}

enum Destination<R> {
    Channel {
        tx: std::sync::mpsc::Sender<OscEvent>,
        repaint: R,
    },
    #[cfg(test)]
    Collected(Vec<OscEvent>),
}

pub(crate) struct Sink<R> {
    policy: TapPolicy,
    destination: Destination<R>,
    pending: usize,
    scan_state: ScanState,
}

#[cfg(test)]
impl Sink<crate::repaint::Recorder> {
    fn collecting(policy: TapPolicy) -> Self {
        Self {
            policy,
            destination: Destination::Collected(Vec::new()),
            pending: 0,
            scan_state: ScanState::Ground,
        }
    }

    fn take(&mut self) -> Vec<OscEvent> {
        match &mut self.destination {
            Destination::Collected(events) => std::mem::take(events),
            Destination::Channel { .. } => unreachable!("collecting sink only"),
        }
    }
}

impl<R: Repaint> Sink<R> {
    fn channel(policy: TapPolicy, tx: std::sync::mpsc::Sender<OscEvent>, repaint: R) -> Self {
        Self {
            policy,
            destination: Destination::Channel { tx, repaint },
            pending: 0,
            scan_state: ScanState::Ground,
        }
    }

    fn reset_pending(&mut self) {
        self.pending = 0;
        self.scan_state = ScanState::Ground;
    }

    fn scan(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            match self.scan_state {
                ScanState::Ground => {
                    if byte == 0x1b {
                        self.scan_state = ScanState::Escape;
                    }
                },
                ScanState::Escape => match byte {
                    0x5d => {
                        self.pending = 0;
                        self.scan_state = ScanState::Osc;
                    },
                    0x1b => {},
                    0x18 | 0x1a => self.scan_state = ScanState::Ground,
                    0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f..=0xff => {},
                    _ => self.scan_state = ScanState::Ground,
                },
                ScanState::Osc => match byte {
                    0x07 | 0x18 | 0x1a => self.reset_pending(),
                    0x1b => {
                        self.pending = 0;
                        self.scan_state = ScanState::Escape;
                    },
                    _ => self.pending = self.pending.saturating_add(1),
                },
            }
        }
    }

    fn emit(&mut self, event: OscEvent) {
        match &mut self.destination {
            Destination::Channel { tx, repaint } => {
                if tx.send(event).is_ok() {
                    repaint.wake();
                }
            },
            #[cfg(test)]
            Destination::Collected(events) => events.push(event),
        }
    }
}

impl<R: Repaint> vte::Perform for Sink<R> {
    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        self.pending = 0;
        if let Some(event) = classify(params, &self.policy) {
            self.emit(event);
        }
    }
}

/// Feeds one chunk, resetting first when the stream had a hole in it.
pub(crate) fn feed<R: Repaint>(
    parser: &mut vte::Parser,
    sink: &mut Sink<R>,
    chunk: &crate::pty_tee::Chunk,
) {
    if chunk.gap_before {
        *parser = vte::Parser::new();
        sink.reset_pending();
    }
    parser.advance(sink, &chunk.bytes);
    sink.scan(&chunk.bytes);
    if sink.pending > MAX_PENDING {
        *parser = vte::Parser::new();
        sink.reset_pending();
    }
}

/// Starts the parser thread for a session when any OSC feature is enabled.
pub(crate) fn spawn<R: Repaint>(
    policy: TapPolicy,
    repaint: R,
) -> Option<(crate::pty_tee::TapHandle, std::sync::mpsc::Receiver<OscEvent>)> {
    if !policy.vt.any_enabled() {
        return None;
    }

    let (chunk_tx, chunk_rx) = std::sync::mpsc::sync_channel(crate::pty_tee::QUEUE_DEPTH);
    let (pool_tx, pool_rx) = std::sync::mpsc::channel();
    let (event_tx, event_rx) = std::sync::mpsc::channel();

    let started = std::thread::Builder::new().name("alacritree-osc-tap".into()).spawn(move || {
        let mut parser = vte::Parser::new();
        let mut sink = Sink::channel(policy, event_tx, repaint);
        while let Ok(chunk) = chunk_rx.recv() {
            feed(&mut parser, &mut sink, &chunk);
            let _ = pool_tx.send(chunk.bytes);
        }
    });

    match started {
        Ok(_) => Some((crate::pty_tee::TapHandle::new(chunk_tx, pool_rx), event_rx)),
        Err(error) => {
            log::debug!("osc tap thread did not start: {error}");
            None
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(shell: ShellPlatform) -> TapPolicy {
        TapPolicy {
            vt: VtConfig { report_cwd: true, notify: true, progress: true, pointer_shape: true },
            hostname: "LEVPC".to_string(),
            shell,
        }
    }

    fn osc(payload: &[&str], shell: ShellPlatform) -> Option<OscEvent> {
        let owned: Vec<&[u8]> = payload.iter().map(|s| s.as_bytes()).collect();
        classify(&owned, &policy(shell))
    }

    #[test]
    fn osc7_accepts_every_spelling_of_a_local_host() {
        for host in ["", "localhost", "levpc", "LEVPC"] {
            assert_eq!(
                osc(&["7", &format!("file://{host}/home/dev/src")], ShellPlatform::Unix),
                Some(OscEvent::Cwd(Some("/home/dev/src".into()))),
                "host {host:?}",
            );
        }
    }

    #[test]
    fn osc7_rejects_a_foreign_host_and_a_foreign_scheme() {
        assert_eq!(osc(&["7", "file://remote/home/dev"], ShellPlatform::Unix), None);
        assert_eq!(osc(&["7", "http://localhost/home/dev"], ShellPlatform::Unix), None);
    }

    #[test]
    fn osc7_decodes_percent_escapes() {
        assert_eq!(
            osc(&["7", "file:///home/dev/my%20src"], ShellPlatform::Unix),
            Some(OscEvent::Cwd(Some("/home/dev/my src".into()))),
        );
    }

    #[test]
    fn osc7_strips_the_slash_before_a_drive_letter() {
        assert_eq!(
            osc(&["7", "file:///C:/src"], ShellPlatform::Windows),
            Some(OscEvent::Cwd(Some("C:/src".into()))),
        );
    }

    #[test]
    fn osc7_treats_an_empty_payload_as_a_reset() {
        assert_eq!(osc(&["7", ""], ShellPlatform::Unix), Some(OscEvent::Cwd(None)));
    }

    #[test]
    fn a_unc_path_is_refused_by_both_routes() {
        assert_eq!(osc(&["9", "9", r"\\evil\share"], ShellPlatform::Windows), None);
        assert_eq!(osc(&["7", "file:////evil/share"], ShellPlatform::Windows), None);
    }

    #[test]
    fn an_unrooted_path_is_refused() {
        for path in ["src/thing", "~/src", "C:thing"] {
            assert_eq!(osc(&["9", "9", path], ShellPlatform::Windows), None, "path {path:?}");
        }
    }

    #[test]
    fn osc9_9_strips_one_pair_of_quotes_and_keeps_the_remainder() {
        assert_eq!(
            osc(&["9", "9", "\"D:/src\""], ShellPlatform::Windows),
            Some(OscEvent::Cwd(Some("D:/src".into()))),
        );
        // vte split the path on its semicolon; rejoining is what keeps it.
        assert_eq!(
            osc(&["9", "9", "D:/a", "b"], ShellPlatform::Windows),
            Some(OscEvent::Cwd(Some("D:/a;b".into()))),
        );
    }

    #[test]
    fn osc9_tells_a_conemu_subcommand_from_a_notification() {
        assert_eq!(
            osc(&["9", "9 items remaining"], ShellPlatform::Unix),
            Some(OscEvent::Notify("9 items remaining".into())),
        );
        assert_eq!(
            osc(&["9", "build finished"], ShellPlatform::Unix),
            Some(OscEvent::Notify("build finished".into())),
        );
    }

    #[test]
    fn osc9_drops_unsupported_digit_run_subcommands() {
        for payload in [
            &["9", "1", "500"][..],
            &["9", "2", "msg"][..],
            &["9", "3", "x"][..],
            &["9", "5"][..],
            &["9", "11", "x"][..],
            &["9", "12"][..],
            &["9", "4"][..],
        ] {
            assert_eq!(osc(payload, ShellPlatform::Unix), None, "payload {payload:?}");
        }
    }

    #[test]
    fn osc777_takes_the_body_after_the_title() {
        assert_eq!(
            osc(&["777", "notify", "cargo", "build failed"], ShellPlatform::Unix),
            Some(OscEvent::Notify("build failed".into())),
        );
    }

    #[test]
    fn osc9_4_follows_windows_terminals_bounds() {
        let unix = ShellPlatform::Unix;
        assert_eq!(osc(&["9", "4", "0"], unix), Some(OscEvent::Progress(Progress::Clear)));
        assert_eq!(osc(&["9", "4", "1", "50"], unix), Some(OscEvent::Progress(Progress::Set(50))));
        assert_eq!(
            osc(&["9", "4", "1", "900"], unix),
            Some(OscEvent::Progress(Progress::Set(100)))
        );
        assert_eq!(osc(&["9", "4", "3"], unix), Some(OscEvent::Progress(Progress::Indeterminate)));
        assert_eq!(osc(&["9", "4", ""], unix), Some(OscEvent::Progress(Progress::Clear)));
        assert_eq!(osc(&["9", "4", "5"], unix), None);
    }

    #[test]
    fn osc9_4_rejects_malformed_present_fields() {
        for payload in
            [&["9", "4", "bad"][..], &["9", "4", "1", "bad"][..], &["9", "4", "3", "bad"][..]]
        {
            assert_eq!(osc(payload, ShellPlatform::Unix), None, "payload {payload:?}");
        }
    }

    #[test]
    fn osc22_maps_known_names_and_ignores_the_rest() {
        assert_eq!(
            osc(&["22", "pointer"], ShellPlatform::Unix),
            Some(OscEvent::PointerShape(egui::CursorIcon::PointingHand)),
        );
        assert_eq!(osc(&["22", "no-such-cursor"], ShellPlatform::Unix), None);
    }

    #[test]
    fn a_disabled_key_silences_its_sequence() {
        let mut p = policy(ShellPlatform::Unix);
        p.vt.report_cwd = false;
        let owned: Vec<&[u8]> = vec![b"7", b"file:///home/dev"];
        assert_eq!(classify(&owned, &p), None);
    }

    fn chunk(bytes: &[u8], gap_before: bool) -> crate::pty_tee::Chunk {
        crate::pty_tee::Chunk { bytes: bytes.to_vec(), gap_before }
    }

    #[test]
    fn a_sequence_split_across_chunks_still_dispatches() {
        let mut parser = vte::Parser::new();
        let mut sink = Sink::collecting(policy(ShellPlatform::Unix));

        feed(&mut parser, &mut sink, &chunk(b"\x1b]7;file:///home/", false));
        assert!(sink.take().is_empty(), "nothing is complete yet");

        feed(&mut parser, &mut sink, &chunk(b"dev/src\x1b\\", false));
        assert_eq!(sink.take(), vec![OscEvent::Cwd(Some("/home/dev/src".into()))]);
    }

    #[test]
    fn ordinary_output_before_a_split_sequence_does_not_reset_the_parser() {
        let mut parser = vte::Parser::new();
        let mut sink = Sink::collecting(policy(ShellPlatform::Unix));
        let mut prefix = vec![b'x'; MAX_PENDING + 1];
        prefix.extend_from_slice(b"\x1b]7;file:///home/");

        feed(&mut parser, &mut sink, &chunk(&prefix, false));
        assert!(sink.take().is_empty(), "nothing is complete yet");

        feed(&mut parser, &mut sink, &chunk(b"dev/src\x1b\\", false));
        assert_eq!(sink.take(), vec![OscEvent::Cwd(Some("/home/dev/src".into()))]);
    }

    #[test]
    fn osc_terminations_reset_the_pending_byte_bound() {
        for terminator in [0x07, 0x18, 0x1a, 0x1b] {
            let mut parser = vte::Parser::new();
            let mut sink = Sink::collecting(policy(ShellPlatform::Unix));
            let mut prefix = b"\x1b]0;ignored".to_vec();
            prefix.push(terminator);
            prefix.extend(vec![b'x'; MAX_PENDING + 1]);
            prefix.extend_from_slice(b"\x1b]7;file:///home/");

            feed(&mut parser, &mut sink, &chunk(&prefix, false));
            assert!(sink.take().is_empty(), "nothing is complete yet");

            feed(&mut parser, &mut sink, &chunk(b"dev/src\x1b\\", false));
            assert_eq!(sink.take(), vec![OscEvent::Cwd(Some("/home/dev/src".into()))]);
        }
    }

    #[test]
    fn a_gap_discards_the_sequence_it_interrupted() {
        let mut parser = vte::Parser::new();
        let mut sink = Sink::collecting(policy(ShellPlatform::Unix));

        feed(&mut parser, &mut sink, &chunk(b"\x1b]7;file:///home/", false));
        feed(&mut parser, &mut sink, &chunk(b"dev/src\x1b\\", true));
        assert!(sink.take().is_empty(), "a hole in the stream must not be framed across");

        feed(&mut parser, &mut sink, &chunk(b"\x1b]7;file:///tmp\x1b\\", false));
        assert_eq!(sink.take(), vec![OscEvent::Cwd(Some("/tmp".into()))]);
    }

    #[test]
    fn an_unterminated_sequence_does_not_grow_without_bound() {
        let mut parser = vte::Parser::new();
        let mut sink = Sink::collecting(policy(ShellPlatform::Unix));

        feed(&mut parser, &mut sink, &chunk(b"\x1b]7;", false));
        let flood = vec![b'x'; MAX_PENDING + 1];
        feed(&mut parser, &mut sink, &chunk(&flood, false));

        feed(&mut parser, &mut sink, &chunk(b"\x1b]7;file:///tmp\x1b\\", false));
        assert_eq!(sink.take(), vec![OscEvent::Cwd(Some("/tmp".into()))]);
    }
}
