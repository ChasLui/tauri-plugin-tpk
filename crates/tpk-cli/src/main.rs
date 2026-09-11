//! `tpk` — pack, sign, verify and publish TPK content packs.
#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod cmd_channel;
mod cmd_inspect;
mod cmd_keygen;
mod cmd_pack;
mod cmd_sign;
mod cmd_verify;
mod html_policy;
mod key_source;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Exit code for a document that failed validation or verification.
pub const EXIT_VERIFICATION_FAILED: u8 = 2;
/// Exit code for bad arguments or unusable inputs.
pub const EXIT_USAGE: u8 = 3;

#[derive(Parser)]
#[command(name = "tpk", version, about = "Build and verify TPK content packs")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate an unencrypted minisign key pair.
    Keygen(cmd_keygen::Args),
    /// Build a signed pack from a directory.
    Pack(cmd_pack::Args),
    /// Sign a file, producing a detached `.minisig`.
    Sign(cmd_sign::Args),
    /// Build and sign the channel manifest.
    Channel(cmd_channel::Args),
    /// Print a pack's manifest. Does not verify the signature.
    Inspect(cmd_inspect::Args),
    /// Verify packs and channel manifests against a public key.
    Verify(cmd_verify::Args),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Keygen(args) => cmd_keygen::run(&args),
        Command::Pack(args) => cmd_pack::run(&args),
        Command::Sign(args) => cmd_sign::run(&args),
        Command::Channel(args) => cmd_channel::run(&args),
        Command::Inspect(args) => cmd_inspect::run(&args),
        Command::Verify(args) => cmd_verify::run(&args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {}", err.message);
            ExitCode::from(err.exit_code)
        }
    }
}

/// A CLI failure carrying the exit code the specification assigns to it.
#[derive(Debug)]
pub struct CliError {
    /// Message shown to the operator on stderr.
    pub message: String,
    /// Process exit code.
    pub exit_code: u8,
}

impl CliError {
    /// A validation or verification failure (exit code 2).
    pub fn verification(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            exit_code: EXIT_VERIFICATION_FAILED,
        }
    }

    /// A usage or input problem (exit code 3).
    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            exit_code: EXIT_USAGE,
        }
    }
}

/// Result alias for subcommands.
pub type CliResult = std::result::Result<(), CliError>;
