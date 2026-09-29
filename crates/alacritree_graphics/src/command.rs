//! The control data of one kitty graphics command, parsed the way kitty's
//! generated `parse_graphics_code` parses it.
//!
//! A malformed command is dropped without a reply, as kitty drops it, so
//! [`ParseError`] only reaches the log.  Its messages are kitty's.

/// What a command asks for, from its `a` key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// `t`, and a command with no `a` at all.
    Transmit,
    /// `T`: transmit, then place at the cursor.
    TransmitAndPut,
    /// `q`: validate a transmission and store nothing.
    Query,
    /// `p`: place an image that was transmitted before.
    Put,
    /// `d`
    Delete,
    /// `f`, `a`, `c`: animation, which this crate does not implement.
    Animation,
}

/// Where the image data comes from, from the `t` key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Medium {
    #[default]
    Direct,
    File,
    TempFile,
    SharedMemory,
}

/// Every key kitty accepts.  Keys this crate has no use for are parsed so a
/// command carrying them is not dropped as malformed.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Command {
    pub action: Option<Action>,
    /// The `d` letter as sent, since its case decides whether images are
    /// freed.  0 when absent.
    pub delete: u8,
    pub medium: Option<Medium>,
    pub compressed: bool,
    pub format: u32,
    pub more: u32,
    pub id: u32,
    pub number: u32,
    pub placement: u32,
    pub quiet: u32,
    pub width: u32,
    pub height: u32,
    pub x: u32,
    pub y: u32,
    pub data_height: u32,
    pub data_width: u32,
    pub data_size: u32,
    pub data_offset: u32,
    pub columns: u32,
    pub rows: u32,
    pub cell_x: u32,
    pub cell_y: u32,
    pub z: i32,
    pub cursor_movement: u32,
    pub unicode: u32,
    pub parent: u32,
    pub parent_placement: u32,
    pub usage: u32,
    pub parent_x: i32,
    pub parent_y: i32,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ParseError {
    #[error("Malformed GraphicsCommand control block, invalid key character: 0x{0:x}")]
    Key(u8),
    #[error("Malformed GraphicsCommand control block, no = after key, found: 0x{0:x} instead")]
    Equals(u8),
    #[error("Malformed GraphicsCommand control block, unknown flag value for {key}: 0x{value:x}")]
    Flag { key: &'static str, value: u8 },
    #[error("Malformed GraphicsCommand control block, expecting an integer value for key: {0}")]
    Integer(char),
    #[error("Malformed GraphicsCommand control block, number is too large")]
    TooLarge,
    #[error(
        "Malformed GraphicsCommand control block, expecting a , or semi-colon after a value, \
         found: 0x{0:x}"
    )]
    Separator(u8),
    #[error("Malformed GraphicsCommand control block, no = after key")]
    Truncated,
    #[error("Malformed GraphicsCommand control block, expecting an integer value")]
    TruncatedInteger,
    #[error("Malformed GraphicsCommand control block, expecting a flag value")]
    TruncatedFlag,
}

#[derive(Clone, Copy)]
enum Value {
    Flag,
    Unsigned,
    Signed,
}

#[derive(Clone, Copy)]
enum State {
    Key,
    Equals,
    Value(Value),
    AfterValue,
}

fn value_kind(key: u8) -> Option<Value> {
    Some(match key {
        b'a' | b'd' | b't' | b'o' => Value::Flag,
        b'z' | b'H' | b'V' => Value::Signed,
        b'f' | b'm' | b'i' | b'I' | b'p' | b'q' | b'w' | b'h' | b'x' | b'y' | b'v' | b's'
        | b'S' | b'O' | b'c' | b'r' | b'X' | b'Y' | b'C' | b'U' | b'P' | b'Q' | b'N' => {
            Value::Unsigned
        },
        _ => return None,
    })
}

/// Parse `payload`, the APC string after its leading `G`, into a command and
/// its still base64 encoded data.
pub(crate) fn parse(payload: &[u8]) -> Result<(Command, &[u8]), ParseError> {
    let mut command = Command::default();
    let mut pos = 0;
    let mut state = if payload.first() == Some(&b';') { State::AfterValue } else { State::Key };
    let mut key = b'a';

    while pos < payload.len() {
        match state {
            State::Key => {
                key = payload[pos];
                pos += 1;
                value_kind(key).ok_or(ParseError::Key(key))?;
                state = State::Equals;
            },
            State::Equals => {
                if payload[pos] != b'=' {
                    return Err(ParseError::Equals(payload[pos]));
                }
                pos += 1;
                state = State::Value(value_kind(key).expect("the key was checked"));
            },
            State::Value(Value::Flag) => {
                let value = payload[pos];
                pos += 1;
                set_flag(&mut command, key, value)?;
                state = State::AfterValue;
            },
            State::Value(kind) => {
                let negative = matches!(kind, Value::Signed) && payload[pos] == b'-';
                if negative {
                    pos += 1;
                }
                let (value, read) = read_unsigned(&payload[pos..], key)?;
                pos += read;
                match kind {
                    Value::Signed => {
                        let value = value as i32;
                        set_signed(
                            &mut command,
                            key,
                            if negative { value.wrapping_neg() } else { value },
                        );
                    },
                    _ => set_unsigned(&mut command, key, value),
                }
                state = State::AfterValue;
            },
            State::AfterValue => {
                let separator = payload[pos];
                pos += 1;
                match separator {
                    b',' => state = State::Key,
                    b';' => return Ok((command, &payload[pos..])),
                    other => return Err(ParseError::Separator(other)),
                }
            },
        }
    }

    match state {
        State::Equals => Err(ParseError::Truncated),
        State::Value(Value::Flag) => Err(ParseError::TruncatedFlag),
        State::Value(_) => Err(ParseError::TruncatedInteger),
        State::Key | State::AfterValue => Ok((command, &[])),
    }
}

/// At most ten digits, as kitty reads them, so an eleventh digit is a
/// malformed separator rather than a larger number.
fn read_unsigned(digits: &[u8], key: u8) -> Result<(u32, usize), ParseError> {
    let read = digits.iter().take(10).take_while(|byte| byte.is_ascii_digit()).count();
    if read == 0 {
        return Err(ParseError::Integer(key as char));
    }
    let value =
        digits[..read].iter().fold(0u64, |value, digit| value * 10 + u64::from(digit - b'0'));
    let value = u32::try_from(value).map_err(|_| ParseError::TooLarge)?;
    Ok((value, read))
}

fn set_flag(command: &mut Command, key: u8, value: u8) -> Result<(), ParseError> {
    let invalid = |key| Err(ParseError::Flag { key, value });
    match key {
        b'a' => {
            command.action = Some(match value {
                b't' => Action::Transmit,
                b'T' => Action::TransmitAndPut,
                b'q' => Action::Query,
                b'p' => Action::Put,
                b'd' => Action::Delete,
                b'f' | b'a' | b'c' => Action::Animation,
                _ => return invalid("action"),
            })
        },
        b'd' => match value {
            b'a' | b'A' | b'c' | b'C' | b'f' | b'F' | b'i' | b'I' | b'n' | b'N' | b'p' | b'P'
            | b'q' | b'Q' | b'r' | b'R' | b'x' | b'X' | b'y' | b'Y' | b'z' | b'Z' => {
                command.delete = value
            },
            _ => return invalid("delete_action"),
        },
        b't' => {
            command.medium = Some(match value {
                b'd' => Medium::Direct,
                b'f' => Medium::File,
                b't' => Medium::TempFile,
                b's' => Medium::SharedMemory,
                _ => return invalid("transmission_type"),
            })
        },
        b'o' => match value {
            b'z' => command.compressed = true,
            _ => return invalid("compressed"),
        },
        _ => unreachable!("only flag keys reach here"),
    }
    Ok(())
}

fn set_unsigned(command: &mut Command, key: u8, value: u32) {
    let field = match key {
        b'f' => &mut command.format,
        b'm' => &mut command.more,
        b'i' => &mut command.id,
        b'I' => &mut command.number,
        b'p' => &mut command.placement,
        b'q' => &mut command.quiet,
        b'w' => &mut command.width,
        b'h' => &mut command.height,
        b'x' => &mut command.x,
        b'y' => &mut command.y,
        b'v' => &mut command.data_height,
        b's' => &mut command.data_width,
        b'S' => &mut command.data_size,
        b'O' => &mut command.data_offset,
        b'c' => &mut command.columns,
        b'r' => &mut command.rows,
        b'X' => &mut command.cell_x,
        b'Y' => &mut command.cell_y,
        b'C' => &mut command.cursor_movement,
        b'U' => &mut command.unicode,
        b'P' => &mut command.parent,
        b'Q' => &mut command.parent_placement,
        b'N' => &mut command.usage,
        _ => unreachable!("only unsigned keys reach here"),
    };
    *field = value;
}

fn set_signed(command: &mut Command, key: u8, value: i32) {
    match key {
        b'z' => command.z = value,
        b'H' => command.parent_x = value,
        b'V' => command.parent_y = value,
        _ => unreachable!("only signed keys reach here"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(control: &str) -> Command {
        parse(control.as_bytes()).expect(control).0
    }

    fn error(control: &str) -> String {
        parse(control.as_bytes()).expect_err(control).to_string()
    }

    // kitty_tests/parser.py test_graphics_command
    #[test]
    fn keys_parse_as_kitty_parses_them() {
        assert_eq!(parsed(&format!("i={}", u32::MAX)).id, u32::MAX);
        let command = parsed("i=3,p=4");
        assert_eq!((command.id, command.placement), (3, 4));

        let (command, payload) = parse(b"a=t,t=d,s=100,z=-9;WA==").unwrap();
        assert_eq!(command.action, Some(Action::Transmit));
        assert_eq!(command.medium, Some(Medium::Direct));
        assert_eq!((command.data_width, command.z), (100, -9));
        assert_eq!(payload, b"WA==");

        let command = parsed("a=t,t=d,s=100,z=9,q=2");
        assert_eq!((command.z, command.quiet), (9, 2));
        assert_eq!(parsed("N=1").usage, 1);
        assert_eq!(parse(b";AAAA").unwrap().1, b"AAAA");
    }

    #[test]
    fn malformed_control_data_is_rejected_with_kittys_messages() {
        let prefix = "Malformed GraphicsCommand control block, ";
        let cases = [
            (format!("i={}", u64::from(u32::MAX) + 1), "number is too large"),
            (",s=1".into(), "invalid key character: 0x2c"),
            ("W=1".into(), "invalid key character: 0x57"),
            ("1=1".into(), "invalid key character: 0x31"),
            ("a=t,,w=2".into(), "invalid key character: 0x2c"),
            ("s".into(), "no = after key"),
            ("s=".into(), "expecting an integer value"),
            ("s==".into(), "expecting an integer value for key: s"),
            ("s=1=".into(), "expecting a , or semi-colon after a value, found: 0x3d"),
        ];
        for (control, message) in cases {
            assert_eq!(error(&control), format!("{prefix}{message}"), "{control}");
        }
    }

    #[test]
    fn an_eleventh_digit_is_not_part_of_the_number() {
        assert_eq!(
            error("i=12345678901"),
            "Malformed GraphicsCommand control block, expecting a , or semi-colon after a value, \
             found: 0x31"
        );
    }

    #[test]
    fn a_negative_z_wraps_like_kittys_int32_cast() {
        assert_eq!(parsed("z=-1073741825").z, -(1 << 30) - 1);
        assert_eq!(parsed(&format!("z=-{}", u32::MAX)).z, 1);
    }
}
