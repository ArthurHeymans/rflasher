//! CLI argument parsing

use clap::{Parser, Subcommand};
use rflasher_flash::programmer_names_short;
use std::path::PathBuf;

/// Generate dynamic help text for the programmer argument
fn programmer_help() -> String {
    format!(
        "Programmer to use [available: {}]",
        programmer_names_short()
    )
}

#[derive(Parser)]
#[command(name = "rflasher")]
#[command(author, version, about = "Flash chip programmer", long_about = None)]
pub struct Cli {
    /// Verbosity level (-v, -vv, -vvv)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Path to chip database directory (contains .ron files)
    /// Defaults to looking in ./chips/vendors/ and /usr/share/rflasher/chips/
    #[arg(long, global = true)]
    pub chip_db: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

/// Layout options shared across commands
#[derive(clap::Args, Debug, Clone, Default)]
pub struct LayoutArgs {
    /// Layout file (TOML format)
    #[arg(long, conflicts_with_all = ["ifd", "fmap"])]
    pub layout: Option<PathBuf>,

    /// Read layout from Intel Flash Descriptor (IFD) in flash
    #[arg(long, conflicts_with_all = ["layout", "fmap"])]
    pub ifd: bool,

    /// Read layout from FMAP structure in flash
    #[arg(long, conflicts_with_all = ["layout", "ifd"])]
    pub fmap: bool,

    /// Include only these regions (comma-separated, requires layout)
    #[arg(long, value_delimiter = ',')]
    pub include: Vec<String>,

    /// Exclude these regions (comma-separated, requires layout)
    #[arg(long, value_delimiter = ',')]
    pub exclude: Vec<String>,

    /// Operate on a single region (shorthand for --include with one region)
    #[arg(long)]
    pub region: Option<String>,
}

impl LayoutArgs {
    /// Check if any layout source is specified
    #[allow(dead_code)]
    pub fn has_layout_source(&self) -> bool {
        self.layout.is_some() || self.ifd || self.fmap
    }

    /// Check if region filtering is requested
    #[allow(dead_code)]
    pub fn has_region_filter(&self) -> bool {
        !self.include.is_empty() || !self.exclude.is_empty() || self.region.is_some()
    }
}

#[derive(Subcommand)]
pub enum Commands {
    /// Probe for flash chip
    Probe {
        /// Programmer to use
        #[arg(short, long, help = programmer_help())]
        programmer: String,
    },

    /// Read flash contents to file
    Read {
        /// Programmer to use
        #[arg(short, long, help = programmer_help())]
        programmer: String,

        /// Output file path (or directory if using --layout with multiple regions)
        #[arg(short, long)]
        output: PathBuf,

        /// Chip name (optional, auto-detected if not specified)
        #[arg(short, long)]
        chip: Option<String>,

        #[command(flatten)]
        layout: LayoutArgs,
    },

    /// Write file to flash
    ///
    /// When writing with a layout (--ifd, --fmap, or --layout), the input file
    /// is interpreted based on its size:
    ///
    /// - Multiple regions: File must be full chip size. Data is extracted from
    ///   the file at each region's offset.
    ///
    /// - Single region with file == chip size: Full chip image, region data
    ///   extracted from file at region offset.
    ///
    /// - Single region with file <= region size: Region file, written starting
    ///   at the region's base address. If smaller than the region, only that
    ///   portion is written.
    ///
    /// - Single region with region size < file < chip size: Error (ambiguous).
    Write {
        /// Programmer to use
        #[arg(short, long, help = programmer_help())]
        programmer: String,

        /// Input file path (see command help for size requirements with layouts)
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

        #[command(flatten)]
        layout: LayoutArgs,
    },

    /// Erase flash chip
    Erase {
        /// Programmer to use
        #[arg(short, long, help = programmer_help())]
        programmer: String,

        /// Chip name (optional, auto-detected if not specified)
        #[arg(short, long)]
        chip: Option<String>,

        #[command(flatten)]
        layout: LayoutArgs,
    },

    /// Verify flash contents against file
    Verify {
        /// Programmer to use
        #[arg(short, long, help = programmer_help())]
        programmer: String,

        /// Input file path to verify against
        #[arg(short, long)]
        input: PathBuf,

        /// Chip name (optional, auto-detected if not specified)
        #[arg(short, long)]
        chip: Option<String>,

        #[command(flatten)]
        layout: LayoutArgs,
    },

    /// Show chip information
    Info {
        /// Programmer to use
        #[arg(short, long, help = programmer_help())]
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

    /// Layout operations
    #[command(subcommand)]
    Layout(LayoutCommands),
}

/// Layout-related subcommands
#[derive(Subcommand)]
pub enum LayoutCommands {
    /// Show layout from a file
    Show {
        /// Layout file (TOML format)
        #[arg(short, long)]
        file: PathBuf,
    },

    /// Extract layout from flash image (IFD or FMAP)
    Extract {
        /// Input file (flash image)
        #[arg(short, long)]
        input: PathBuf,

        /// Output layout file (TOML format)
        #[arg(short, long)]
        output: PathBuf,
    },

    /// Extract Intel Flash Descriptor layout from image
    Ifd {
        /// Input file (flash image)
        #[arg(short, long)]
        input: PathBuf,

        /// Output layout file (TOML format, optional - prints to stdout if not specified)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Extract FMAP layout from image
    Fmap {
        /// Input file (flash image)
        #[arg(short, long)]
        input: PathBuf,

        /// Output layout file (TOML format, optional - prints to stdout if not specified)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Create a new layout file template
    Create {
        /// Output layout file
        #[arg(short, long)]
        output: PathBuf,

        /// Chip size (e.g., "16 MiB", "0x1000000")
        #[arg(long)]
        size: String,
    },
}
