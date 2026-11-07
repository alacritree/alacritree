//! The words for what an agent is doing. A program's own OSC 7501 report and
//! a multiplexer's status query use the same words, so both read into this
//! one type.

/// What an agent is doing, ordered by how much it wants a human. That order
/// is how a session with several reports picks the one its row shows. Each
/// variant is spelled as OSC 7501 and herdr spell it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, strum::EnumString)]
#[strum(serialize_all = "lowercase")]
pub enum AgentStatus {
    /// An agent is there, and its word for its state is one alacritree does
    /// not know. Only a multiplexer says this. An OSC 7501 report with an
    /// unknown state is ignored, so parsing never produces it.
    #[default]
    #[strum(disabled)]
    Unknown,
    Idle,
    Working,
    Done,
    /// The program failed. It ranks above `done`, since a failure is the
    /// result to look at first.
    Error,
    Blocked,
}

impl AgentStatus {
    /// The word a row paints for this status.
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Error => "error",
            Self::Blocked => "blocked",
        }
    }

    /// Whether the agent has finished and left a result to look at.
    pub fn is_result(self) -> bool {
        matches!(self, Self::Done | Self::Error)
    }
}

#[cfg(test)]
mod tests {
    use super::AgentStatus;

    #[test]
    fn a_status_parses_from_its_label_except_unknown() {
        use AgentStatus::*;
        for status in [Idle, Working, Done, Error, Blocked] {
            assert_eq!(status.label().parse(), Ok(status));
        }
        assert!("unknown".parse::<AgentStatus>().is_err());
        assert!("coffee".parse::<AgentStatus>().is_err());
    }
}
