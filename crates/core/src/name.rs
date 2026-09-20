use crate::{Error, Result};
use rand::seq::SliceRandom;
use std::ffi::OsStr;
use std::path::Path;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RiftName(String);

impl RiftName {
    pub(crate) fn new(name: String) -> Result<Self> {
        let single_segment = Path::new(&name).file_name() == Some(OsStr::new(&name));
        if !single_segment || name.starts_with('.') {
            return Err(Error::Path(format!("invalid rift name: {name}")));
        }
        Ok(Self(name))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
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
