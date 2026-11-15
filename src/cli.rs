//! Command line interface.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};

use crate::config::{Config, ConfigError};
use crate::diagnostics::Exit;
use crate::discovery::{Limits, OverlapError};
use crate::inventory::Inventory;

#[derive(Debug, Parser)]
#[command(name = "agenvil", version, about)]
pub struct Cli {
    /// Project root containing agenvil.toml [default: current directory]
    #[arg(long, global = true, value_name = "DIR")]
    pub root: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Validate the source tree (read-only, offline)
    Check,
    /// List the discovered source resources (read-only, offline)
    Inventory {
        #[arg(long, value_enum, default_value_t = Format::Json)]
        format: Format,
    },
    /// List the validation rules (read-only)
    Rules,
    /// Explain one finding ID (read-only)
    Explain {
        /// Stable finding ID
        id: String,
    },
    /// Build staged output; never touches ~/.claude (mutating)
    Build,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Format {
    Json,
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Inventory { .. } => "inventory",
            Self::Rules => "rules",
            Self::Explain { .. } => "explain",
            Self::Build => "build",
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("`agenvil {0}` is not implemented yet, so nothing was verified")]
    NotImplemented(&'static str),
    #[error(transparent)]
    Overlap(#[from] OverlapError),
    #[error("could not serialize the inventory: {0}")]
    Serialize(#[from] serde_json::Error),
    /// The inventory was printed, but problems mean it is incomplete.
    #[error("{0} problem(s) found; the inventory above is incomplete")]
    Incomplete(usize),
}

impl Error {
    fn exit(&self) -> Exit {
        match self {
            Self::Config(_) | Self::Overlap(_) => Exit::Usage,
            Self::NotImplemented(_) | Self::Incomplete(_) => Exit::Unverifiable,
            Self::Serialize(_) => Exit::Internal,
        }
    }
}

/// Runs the command, reporting errors on stderr.
pub fn run(cli: &Cli) -> Exit {
    match execute(cli) {
        Ok(()) => Exit::Success,
        Err(error) => {
            eprintln!("error: {error}");
            error.exit()
        }
    }
}

fn execute(cli: &Cli) -> Result<(), Error> {
    let root = cli.root.as_deref().unwrap_or(Path::new("."));
    // Load first so a broken config is never mistaken for a clean or merely unimplemented run.
    let config = Config::load(root)?;
    match &cli.command {
        Command::Inventory {
            format: Format::Json,
        } => {
            let inventory = Inventory::build(root, &config, &Limits::default())?;
            print!("{}", inventory.to_json()?);
            // Problems are untrusted-input failures, not policy findings, so this is "no verdict".
            match inventory.problems.len() {
                0 => Ok(()),
                n => Err(Error::Incomplete(n)),
            }
        }
        // Not implemented yet; none may claim success.
        command => Err(Error::NotImplemented(command.name())),
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn config_errors_exit_usage_and_valid_config_is_unverifiable() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["agenvil", "--root", dir.path().to_str().unwrap(), "check"]);
        assert_eq!(run(&cli), Exit::Usage);

        std::fs::write(
            dir.path().join("agenvil.toml"),
            "schema = 1\n[project]\nname = \"x\"\n",
        )
        .unwrap();
        assert_eq!(run(&cli), Exit::Unverifiable);
    }
}
