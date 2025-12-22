//! CLI argument parsing

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "rflasher")]
#[command(author, version, about = "Flash chip programmer", long_about = None)]
pub struct Cli {
    /// Verbosity level (-v, -vv, -vvv)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Probe for flash chip
    Probe {
        /// Programmer to use (e.g., "dummy", "ch341a", "serprog:dev=/dev/ttyUSB0")
        #[arg(short, long)]
        programmer: String,
    },

    /// Read flash contents to file
    Read {
        /// Programmer to use
        #[arg(short, long)]
        programmer: String,

        /// Output file path
        #[arg(short, long)]
        output: PathBuf,

        /// Chip name (optional, auto-detected if not specified)
        #[arg(short, long)]
        chip: Option<String>,
    },

    /// Write file to flash
    Write {
        /// Programmer to use
        #[arg(short, long)]
        programmer: String,

        /// Input file path
        #[arg(short, long)]
        input: PathBuf,

        /// Chip name (optional, auto-detected if not specified)
        #[arg(short, long)]
        chip: Option<String>,

        /// Verify after writing
        #[arg(long, default_value = "true")]
        verify: bool,

        /// Don't erase before writing
        #[arg(long)]
        no_erase: bool,
    },

    /// Erase flash chip
    Erase {
        /// Programmer to use
        #[arg(short, long)]
        programmer: String,

        /// Chip name (optional, auto-detected if not specified)
        #[arg(short, long)]
        chip: Option<String>,
    },

    /// Verify flash contents against file
    Verify {
        /// Programmer to use
        #[arg(short, long)]
        programmer: String,

        /// Input file path to verify against
        #[arg(short, long)]
        input: PathBuf,

        /// Chip name (optional, auto-detected if not specified)
        #[arg(short, long)]
        chip: Option<String>,
    },

    /// Show chip information
    Info {
        /// Programmer to use
        #[arg(short, long)]
        programmer: String,

        /// Chip name (optional, auto-detected if not specified)
        #[arg(short, long)]
        chip: Option<String>,
    },

    /// List supported programmers
    ListProgrammers,

    /// List supported chips
    ListChips {
        /// Filter by vendor
        #[arg(long)]
        vendor: Option<String>,
    },
}
