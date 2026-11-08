/// Side-effect class of a host tool.
///
/// Untagged tools are [`Effect::Irreversible`]. Pure and read calls run as soon
/// as their inputs are ready. Reversible calls run once earlier observable
/// work they could conflict with has finished. Irreversible calls also wait
/// until the whole program has been received.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Effect {
    Pure,
    Read,
    Reversible,
    #[default]
    Irreversible,
}

impl Effect {
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "pure" => Some(Self::Pure),
            "read" => Some(Self::Read),
            "reversible" => Some(Self::Reversible),
            "irreversible" => Some(Self::Irreversible),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pure => "pure",
            Self::Read => "read",
            Self::Reversible => "reversible",
            Self::Irreversible => "irreversible",
        }
    }
}
