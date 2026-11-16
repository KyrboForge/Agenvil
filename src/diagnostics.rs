//! Process exit codes and the problems commands report about untrusted input.
//!
//! A [`Problem`] says "this file could not be read or parsed". It is not a policy finding:
//! whether a readable resource is acceptable (required fields, allowed models, ...) is decided
//! later by the validator.

use serde::Serialize;
use std::panic::{UnwindSafe, catch_unwind};

/// Stable exit codes. Changing a value is a breaking change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Exit {
    Success = 0,
    /// Validation ran and reported policy findings.
    Findings = 1,
    /// Bad command line or unusable configuration.
    Usage = 2,
    /// Bug in Agenvil, including panics.
    Internal = 3,
    /// No verdict could be produced (for example, a command that is not implemented yet).
    Unverifiable = 4,
}

impl From<Exit> for std::process::ExitCode {
    fn from(exit: Exit) -> Self {
        Self::from(exit as u8)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProblemKind {
    /// A symlink was found where a file or directory is expected. Never followed or read.
    Symlink,
    /// Something other than a regular file (FIFO, socket, device) where a file is expected.
    NotRegularFile,
    TooLarge,
    Unreadable,
    InvalidUtf8,
    /// A path that is not valid UTF-8 and so cannot be reported portably.
    InvalidPath,
    /// A skill directory without a `SKILL.md`.
    MissingSkillFile,
    /// A configured directory (or one of its parents) exists but is not a directory.
    NotADirectory,
    /// A scan limit was hit; the inventory is incomplete.
    LimitExceeded,
    /// Malformed YAML frontmatter.
    Frontmatter,
    /// A file a selected rule needs does not exist.
    Missing,
}

/// Something wrong with one input file, located by a path relative to the project root.
/// Field order is the sort order, so sorted problems are stable.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Problem {
    /// Relative to the project root, `/`-separated on every OS.
    pub path: String,
    /// 1-based line within the file, when known.
    pub line: Option<usize>,
    pub kind: ProblemKind,
    pub message: String,
}

/// Runs `f`, mapping a panic to [`Exit::Internal`] instead of Rust's default 101.
pub fn guard(f: impl FnOnce() -> Exit + UnwindSafe) -> Exit {
    catch_unwind(f).unwrap_or(Exit::Internal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_stable() {
        let codes = [
            Exit::Success,
            Exit::Findings,
            Exit::Usage,
            Exit::Internal,
            Exit::Unverifiable,
        ];
        assert_eq!(codes.map(|e| e as u8), [0, 1, 2, 3, 4]);
    }

    #[test]
    fn guard_passes_through_and_maps_panics_to_internal() {
        assert_eq!(guard(|| Exit::Findings), Exit::Findings);
        assert_eq!(guard(|| panic!("boom")), Exit::Internal);
    }
}
