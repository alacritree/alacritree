//! One call to a distro's core through its resident WSL helper, which costs
//! no `wsl.exe` launch.
//!
//! The helper runs a script and returns its stdout once the script ends, so
//! it carries no stream. The script pipes `hello` and one request into a
//! plain-mode `hc connect`, which answers both and exits when its input
//! closes.

use serde_json::{Value, json};

use crate::CoreError;
use crate::connection::GENERATION;

/// The arguments are the socket for `pin_socket!`, the `hc` program, the
/// hello and the request. stderr joins stdout, because the helper keeps only
/// stdout and a shell that cannot find `hc` says so on stderr.
pub(crate) const SCRIPT: &str =
    concat!(pin_socket!(), r#"printf '%s\n%s\n' "$2" "$3" | "$1" connect 2>&1"#);

/// The request's id, which picks its answer out of the output.
const ID: u64 = 1;

/// The script's arguments after the program: the hello and the request.
pub(crate) fn lines(method: &str, params: Value) -> [String; 2] {
    let hello = json!({ "jsonrpc": "2.0", "id": 0, "method": "hello",
                        "params": { "generation": GENERATION } });
    let request = json!({ "jsonrpc": "2.0", "id": ID, "method": method, "params": params });
    [hello.to_string(), request.to_string()]
}

/// The call's result from what the script printed. A refusal before the
/// request, such as a core that listens elsewhere, comes as an error line
/// with no id, and a shell that found no `hc` as plain text.
pub(crate) fn answer(output: &[u8]) -> Result<Value, CoreError> {
    let text = String::from_utf8_lossy(output);
    let replies: Vec<Value> =
        text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect();
    if let Some(reply) = replies.iter().find(|reply| reply["id"] == ID) {
        return match reply.get("error") {
            Some(error) => Err(CoreError::rpc(error)),
            None => Ok(reply["result"].clone()),
        };
    }
    match replies.iter().find_map(|reply| reply.get("error")) {
        Some(error) => Err(CoreError::rpc(error)),
        None => Err(CoreError::Refused(text.trim().to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELLO_REPLY: &str =
        r#"{"id":0,"jsonrpc":"2.0","result":{"framed":false,"generation":1}}"#;

    #[test]
    fn the_requests_reply_is_the_answer() {
        let output = format!(
            "{HELLO_REPLY}\n{}\n",
            r#"{"id":1,"jsonrpc":"2.0","result":{"address":"t1","pid":42}}"#
        );

        let result = answer(output.as_bytes()).unwrap();

        assert_eq!(result["address"], "t1");
        assert_eq!(result["pid"], 42);
    }

    /// The core's refusal of the call reaches the notice with its kind.
    #[test]
    fn a_refused_call_names_the_cores_reason() {
        let output = format!(
            "{HELLO_REPLY}\n{}\n",
            r#"{"error":{"code":-32000,"data":{"kind":"not_found"},"message":"no terminal \"t9\""},"id":1,"jsonrpc":"2.0"}"#
        );

        let error = answer(output.as_bytes()).unwrap_err();

        assert!(
            matches!(&error, CoreError::Rpc { kind, message, .. }
                if kind == "not_found" && message.contains("t9")),
            "{error:?}"
        );
    }

    /// `hc connect` refuses before it reads the request when the core
    /// listens at another socket. Its hint is what the user needs.
    #[test]
    fn a_core_that_listens_elsewhere_is_named_in_the_error() {
        let output = br#"{"error":{"hint":"set HC_SOCKET to the first path","kind":"unavailable","message":"the core listens at /a.sock, but this shell looks at /b.sock"}}"#;

        let error = answer(output).unwrap_err();

        assert!(
            matches!(&error, CoreError::Rpc { kind, message, hint: Some(hint) }
                if kind == "unavailable" && message.contains("/a.sock") && hint.contains("HC_SOCKET")),
            "{error:?}"
        );
    }

    #[test]
    fn a_distro_without_hc_says_what_its_shell_said() {
        let error = answer(b"sh: 1: hc: not found\n").unwrap_err();

        assert!(
            matches!(&error, CoreError::Refused(text) if text == "sh: 1: hc: not found"),
            "{error:?}"
        );
    }
}
