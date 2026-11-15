//! Process exit codes shared by every command.

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
