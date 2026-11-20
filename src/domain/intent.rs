use serde::{Deserialize, Serialize};

/// Message intent. A closed enum that drives default behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Intent {
    Note,
    Status,
    Alert,
    Question,
    ReviewRequest,
    Handoff,
    Answer,
    Ack,
}

impl Intent {
    pub fn default_disposition(self) -> Disposition {
        match self {
            Intent::Question | Intent::ReviewRequest => Disposition::Request,
            Intent::Handoff => Disposition::Handoff,
            Intent::Answer | Intent::Ack => Disposition::Reply,
            Intent::Note | Intent::Status | Intent::Alert => Disposition::Notify,
        }
    }

    pub fn expects_reply(self) -> bool {
        matches!(self, Intent::Question | Intent::ReviewRequest)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Disposition {
    Notify,
    Request,
    Reply,
    Handoff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WakePolicy {
    Poll,
    Never,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_kebab_case() {
        assert_eq!(
            serde_json::to_string(&Intent::ReviewRequest).unwrap(),
            "\"review-request\""
        );
        assert_eq!(
            serde_json::to_string(&Disposition::Handoff).unwrap(),
            "\"handoff\""
        );
        assert_eq!(
            serde_json::to_string(&WakePolicy::Never).unwrap(),
            "\"never\""
        );
    }

    #[test]
    fn intent_defaults() {
        assert_eq!(Intent::Question.default_disposition(), Disposition::Request);
        assert_eq!(Intent::Handoff.default_disposition(), Disposition::Handoff);
        assert_eq!(Intent::Note.default_disposition(), Disposition::Notify);
        assert!(Intent::Question.expects_reply());
        assert!(!Intent::Note.expects_reply());
    }
}
