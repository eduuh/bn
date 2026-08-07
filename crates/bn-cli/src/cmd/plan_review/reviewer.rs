/// A reviewer specialty. `Generic` is the historical untyped critic; typed kinds select a
/// targeted Markdown prompt lens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReviewerKind {
    Generic,
    RubberDuck,
    CodeReview,
}

impl ReviewerKind {
    pub(super) fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_lowercase().replace('_', "-").as_str() {
            "rubber-duck" | "rubberduck" | "duck" => Some(Self::RubberDuck),
            "code-review" | "codereview" | "code" => Some(Self::CodeReview),
            _ => None,
        }
    }

    pub(super) fn slug(self) -> &'static str {
        match self {
            Self::Generic => "agent",
            Self::RubberDuck => "rubber-duck",
            Self::CodeReview => "code-review",
        }
    }

    pub(super) fn lens(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::RubberDuck => "rubber-duck",
            Self::CodeReview => "code-review",
        }
    }
}

/// A concrete reviewer to spawn: a stable identity plus its specialty.
pub(super) struct ReviewerSpec {
    pub(super) reviewer: String,
    pub(super) kind: ReviewerKind,
}
