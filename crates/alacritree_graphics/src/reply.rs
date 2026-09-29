//! The replies a command gets, byte for byte as kitty's
//! `finish_command_response` writes them.

use crate::MAX_DIMENSION;

/// Why a command failed, as the reply spells it: kitty's `CODE:message`.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum CommandError {
    #[error("EINVAL:Must not specify both image id and image number")]
    IdAndNumber,
    #[error("EINVAL:Image too large, width or height greater than {MAX_DIMENSION}")]
    TooLarge,
    #[error("EINVAL:PNG data size too large")]
    PngDataTooLarge,
    #[error("EINVAL:Zero width/height not allowed")]
    ZeroSize,
    #[error("EINVAL:Unknown image format: {0}")]
    UnknownFormat(u32),
    #[error("EFBIG:Too much data")]
    TooMuchData,
    #[error("EINVAL:Filename too long")]
    FilenameTooLong,
    /// Every failure to read an image file, so a client cannot probe the
    /// filesystem through the replies.
    #[error("EBADF:Failed to read image file")]
    ImageFile,
    #[error("ENODATA:Insufficient image data: {got} < {expected}")]
    InsufficientData { got: usize, expected: usize },
    #[error("EILSEQ:More payload loading refers to non-existent image")]
    OrphanChunk,
    #[error("EINVAL:Failed to inflate image data with error: {0}")]
    Inflate(&'static str),
    #[error("EBADPNG:{0}")]
    BadPng(&'static str),
    #[error("ENOMEM:PNG image is too large")]
    PngTooLarge,
    #[error("ENOMEM:Image is larger than the storage quota")]
    OverQuota,
    #[error("ENOENT:Put command refers to non-existent image with id: {id} and number: {number}")]
    NoImage { id: u32, number: u32 },
    #[error("ENOENT:Put command refers to image with id: {0} that could not load its data")]
    NoData(u32),
    #[error("EINVAL:Put command creating a virtual placement cannot refer to a parent")]
    VirtualWithParent,
    #[error("EINVAL:Relative placements are not supported")]
    Relative,
}

/// The identifiers a reply names and how quiet the client asked it to be.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ReplyTo {
    pub id: u32,
    pub number: u32,
    pub placement: u32,
    pub quiet: u32,
}

/// The reply to send, if any.  `Ok(false)` is a success that has nothing to
/// report yet, such as a chunk that is not the last.
///
/// A command with neither an id nor a number never gets a reply, `q=1`
/// silences successes and `q=2` silences failures too.
pub(crate) fn reply(to: ReplyTo, result: &Result<bool, CommandError>) -> Option<String> {
    use std::fmt::Write;

    match result {
        Ok(false) => return None,
        Ok(true) if to.quiet > 0 => return None,
        Err(_) if to.quiet > 1 => return None,
        _ => (),
    }
    if to.id == 0 && to.number == 0 {
        return None;
    }

    let mut text = String::from("\x1b_G");
    if to.id != 0 {
        let _ = write!(text, "i={}", to.id);
    }
    if to.number != 0 {
        let _ = write!(text, ",I={}", to.number);
    }
    if to.placement != 0 {
        let _ = write!(text, ",p={}", to.placement);
    }
    match result {
        Ok(_) => text.push_str(";OK"),
        Err(error) => {
            let _ = write!(text, ";{error}");
        },
    }
    text.push_str("\x1b\\");
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_name_every_identifier_kitty_names() {
        let to = ReplyTo { id: 1, number: 93, placement: 7, quiet: 0 };
        assert_eq!(reply(to, &Ok(true)).unwrap(), "\x1b_Gi=1,I=93,p=7;OK\x1b\\");
    }

    #[test]
    fn a_number_without_an_id_keeps_kittys_leading_comma() {
        let to = ReplyTo { number: 94, ..Default::default() };
        let error = Err(CommandError::NoImage { id: 0, number: 94 });
        assert_eq!(
            reply(to, &error).unwrap(),
            "\x1b_G,I=94;ENOENT:Put command refers to non-existent image with id: 0 and number: \
             94\x1b\\"
        );
    }

    #[test]
    fn quiet_levels_silence_successes_then_failures() {
        let error = Err(CommandError::ZeroSize);
        let to = |quiet| ReplyTo { id: 1, quiet, ..Default::default() };
        assert!(reply(to(1), &Ok(true)).is_none());
        assert!(reply(to(1), &error).is_some());
        assert!(reply(to(2), &error).is_none());
        assert!(reply(ReplyTo::default(), &error).is_none());
        assert!(reply(to(0), &Ok(false)).is_none());
    }
}
