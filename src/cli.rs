//! CLI argument parsing

use clap::{Parser, Subcommand};
use rflasher_programmers::programmer_names_short;
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

    /// Programmer to use
    #[arg(
        short,
        long,
        global = true,
        env = "RFLASHER_PROGRAMMER",
        help = programmer_help()
    )]
    pub programmer: Option<String>,

    /// Path to chip database directory (contains .ron files)
    /// Defaults to the bundled development database and system data directories.
    #[arg(long, global = true)]
    pub chip_db: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

/// A region selector, optionally paired with a per-region file
/// (`name` or `name:file`).
#[derive(Debug, Clone)]
#[allow(dead_code)] // gen-manpage includes this module but never reads the fields
pub struct RegionSpec {
    pub name: String,
    pub file: Option<PathBuf>,
}

impl std::str::FromStr for RegionSpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (name, file) = match s.split_once(':') {
            Some((name, file)) => (name, Some(PathBuf::from(file))),
            None => (s, None),
        };
        if name.is_empty() {
            return Err(format!("Empty region name in '{}'", s));
        }
        if file.as_ref().is_some_and(|f| f.as_os_str().is_empty()) {
            return Err(format!("Empty file path in '{}'", s));
        }
        Ok(RegionSpec {
            name: name.to_string(),
            file,
        })
    }
}

/// Layout options shared across commands
#[derive(clap::Args, Debug, Clone, Default)]
pub struct LayoutArgs {
    /// Layout file (TOML format)
    #[arg(short, long, conflicts_with_all = ["ifd", "fmap"])]
    pub layout: Option<PathBuf>,

    /// Read layout from Intel Flash Descriptor (IFD) in flash
    #[arg(long, conflicts_with_all = ["layout", "fmap"])]
    pub ifd: bool,

    /// Read layout from FMAP structure in flash
    #[arg(long, conflicts_with_all = ["layout", "ifd"])]
    pub fmap: bool,

    /// Include only these regions (comma-separated NAME[:FILE], requires layout)
    #[arg(long, value_delimiter = ',', value_name = "NAME[:FILE]")]
    pub include: Vec<RegionSpec>,

    /// Exclude these regions (comma-separated, requires layout)
    #[arg(long, value_delimiter = ',')]
    pub exclude: Vec<String>,

    /// Operate on a single region, optionally with its own file
    /// (shorthand for --include with one region)
    #[arg(short, long, value_name = "NAME[:FILE]")]
    pub region: Option<RegionSpec>,
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
    Probe,

    /// Read flash contents to file
    ///
    /// Reads the whole chip (or the selected regions) into FILE. With
    /// --region/--include NAME:FILE, each named region is additionally
    /// written to its own file; FILE may then be omitted.
    #[command(visible_alias = "r")]
    Read {
        /// Output file for the (0xFF-padded) full image. Optional when every
        /// selected region has its own NAME:FILE.
        file: Option<PathBuf>,

        #[command(flatten)]
        layout: LayoutArgs,
    },

    /// Write file to flash
    ///
    /// When writing with a layout (--ifd, --fmap, or --layout), region data
    /// can come from per-region files (--region/--include NAME:FILE) and/or
    /// from FILE:
    ///
    /// - Per-region files: each file is written starting at its region's base
    ///   address and must not exceed the region size. FILE may be omitted if
    ///   every selected region has its own file; if FILE is also given it must
    ///   be a full chip image providing data for the remaining regions.
    ///
    /// - FILE only, multiple regions: FILE must be full chip size. Data is
    ///   extracted from the file at each region's offset.
    ///
    /// - FILE only, single region with file == chip size: full chip image,
    ///   region data extracted from file at region offset.
    ///
    /// - FILE only, single region with file <= region size: region file,
    ///   written starting at the region's base address. If smaller than the
    ///   region, only that portion is written.
    ///
    /// - FILE only, single region with region size < file < chip size:
    ///   Error (ambiguous).
    #[command(visible_alias = "w")]
    Write {
        /// Input file (see command help for size requirements with layouts).
        /// Optional when every selected region has its own NAME:FILE.
        file: Option<PathBuf>,

        /// Skip verification after writing
        #[arg(short, long)]
        no_verify: bool,

        #[command(flatten)]
        layout: LayoutArgs,
    },

    /// Erase flash chip
    #[command(visible_alias = "E")]
    Erase {
        #[command(flatten)]
        layout: LayoutArgs,
    },

    /// Verify flash contents against file
    ///
    /// Size rules mirror `write`, including per-region files via
    /// --region/--include NAME:FILE.
    #[command(visible_alias = "v")]
    Verify {
        /// Input file to verify against. Optional when every selected region
        /// has its own NAME:FILE.
        file: Option<PathBuf>,

        #[command(flatten)]
        layout: LayoutArgs,
    },

    /// Show chip information
    Info,

    /// List supported programmers
    #[command(visible_alias = "programmers")]
    ListProgrammers,

    /// List supported chips
    #[command(visible_alias = "chips")]
    ListChips {
        /// Filter by vendor
        #[arg(long)]
        vendor: Option<String>,
    },

    /// Layout operations
    #[command(subcommand)]
    Layout(LayoutCommands),

    /// Write protection operations
    #[command(subcommand, name = "wp", alias = "write-protect")]
    Wp(WpCommands),

    /// Start Scheme REPL for scripting SPI commands
    #[cfg(feature = "repl")]
    Repl {
        /// Script file to run instead of interactive REPL
        #[arg(short, long)]
        script: Option<std::path::PathBuf>,
    },
}

/// Write protection subcommands
#[derive(Subcommand)]
pub enum WpCommands {
    /// Show current write protection status (default if no subcommand specified)
    Status,

    /// List available protection ranges
    List,

    /// Enable hardware write protection
    Enable,

    /// Disable hardware write protection
    Disable,

    /// Set protection range by address
    Range {
        /// Protection range as "start,length" (e.g., "0,0x100000" or "0x10000,65536")
        range: String,
    },

    /// Set protection range by region name (requires layout)
    Region {
        #[command(flatten)]
        layout: LayoutArgs,

        /// Region name to protect
        region_name: String,
    },
}

/// Layout-related subcommands
#[derive(Subcommand)]
pub enum LayoutCommands {
    /// Show layout from a file
    Show {
        /// Layout file (TOML format)
        file: PathBuf,
    },

    /// Extract layout from flash image (IFD or FMAP)
    Extract {
        /// Input file (flash image)
        input: PathBuf,

        /// Output layout file (TOML format)
        #[arg(short, long)]
        output: PathBuf,
    },

    /// Extract Intel Flash Descriptor layout from image
    Ifd {
        /// Input file (flash image)
        input: PathBuf,

        /// Output layout file (TOML format, optional - prints to stdout if not specified)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Extract FMAP layout from image
    Fmap {
        /// Input file (flash image)
        input: PathBuf,

        /// Output layout file (TOML format, optional - prints to stdout if not specified)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Create a new layout file template
    Create {
        /// Output layout file
        output: PathBuf,

        /// Chip size (e.g., "16 MiB", "0x1000000")
        #[arg(long)]
        size: String,
    },
}
