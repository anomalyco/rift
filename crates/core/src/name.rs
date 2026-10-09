use crate::{Error, Result};
use rand::seq::SliceRandom;
use std::ffi::OsStr;
use std::path::Path;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RiftName(String);

impl RiftName {
    pub(crate) fn new(name: String) -> Result<Self> {
        let single_segment = Path::new(&name).file_name() == Some(OsStr::new(&name));
        if !single_segment || name.starts_with('.') || is_reserved_windows_name(&name) {
            return Err(Error::Path(format!("invalid rift name: {name}")));
        }
        Ok(Self(name))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(not(windows))]
fn is_reserved_windows_name(_name: &str) -> bool {
    false
}

#[cfg(windows)]
fn is_reserved_windows_name(name: &str) -> bool {
    if name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|character| character.is_control() || "<>:\"|?*".contains(character))
    {
        return true;
    }
    let stem = name.split('.').next().unwrap_or(name);
    matches!(
        stem.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

const ADJECTIVES: &[&str] = &[
    "amber", "bold", "brisk", "calm", "cedar", "clear", "cobalt", "coral", "dawn", "ember",
    "gentle", "golden", "jade", "lively", "lunar", "mellow", "misty", "noble", "quiet", "rapid",
    "river", "silver", "solar", "spruce", "steady", "swift", "tidal", "verdant", "violet", "warm",
    "wild", "winter",
];
const NOUNS: &[&str] = &[
    "badger", "brook", "canyon", "cedar", "comet", "dune", "falcon", "field", "forest", "harbor",
    "heron", "island", "lantern", "maple", "meadow", "mesa", "otter", "peak", "pine", "reef",
    "ridge", "robin", "sparrow", "summit", "thicket", "trail", "valley", "willow", "wren",
    "yarrow", "zephyr", "fox",
];

/// Every adjective-noun name in random order, so callers can take the first
/// one that is not already in use.
pub(crate) fn generated() -> impl Iterator<Item = RiftName> {
    let mut names = ADJECTIVES
        .iter()
        .flat_map(|adjective| NOUNS.iter().map(move |noun| format!("{adjective}-{noun}")))
        .collect::<Vec<_>>();
    names.shuffle(&mut rand::rng());
    names.into_iter().map(RiftName)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn names_are_single_path_segments() {
        assert_eq!(RiftName::new("child".into()).unwrap().as_str(), "child");
        assert!(RiftName::new(String::new()).is_err());
        assert!(RiftName::new(".".into()).is_err());
        assert!(RiftName::new("..".into()).is_err());
        assert!(RiftName::new("/".into()).is_err());
        assert!(RiftName::new("parent/child".into()).is_err());
        assert!(RiftName::new("child/".into()).is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn device_names_are_valid_off_windows() {
        assert_eq!(RiftName::new("CON".into()).unwrap().as_str(), "CON");
    }

    #[cfg(windows)]
    #[test]
    fn windows_rejects_device_names_trailing_space_and_forbidden_characters() {
        for name in [
            "CON",
            "con",
            "NUL.txt",
            "COM1",
            "com9.dat",
            "LPT1",
            "prn",
            "AUX.log",
            "file.",
            "file ",
            "a<b",
            "a>b",
            "a:b",
            "a\"b",
            "a|b",
            "a?b",
            "a*b",
            "a\u{0001}b",
        ] {
            assert!(RiftName::new(name.into()).is_err(), "{name}");
        }
        assert_eq!(RiftName::new("child".into()).unwrap().as_str(), "child");
        assert!(RiftName::new("COM10".into()).is_ok());
        assert!(RiftName::new("console".into()).is_ok());
        assert!(RiftName::new("file.CON".into()).is_ok());
    }

    #[test]
    fn names_cannot_be_hidden() {
        assert!(RiftName::new(".trash".into()).is_err());
        assert!(RiftName::new(".hidden".into()).is_err());
    }

    #[test]
    fn generated_names_cover_every_combination_once() {
        let names = generated().collect::<Vec<_>>();
        let unique = names.iter().map(RiftName::as_str).collect::<HashSet<_>>();

        assert_eq!(names.len(), ADJECTIVES.len() * NOUNS.len());
        assert_eq!(unique.len(), names.len());
        assert!(names.iter().all(|name| {
            let parts = name.as_str().split('-').collect::<Vec<_>>();
            parts.len() == 2
                && parts
                    .iter()
                    .all(|part| part.chars().all(|character| character.is_ascii_lowercase()))
        }));
    }
}
