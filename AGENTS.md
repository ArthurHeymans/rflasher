# AGENTS.md - Developer Guide for rflasher

This guide provides essential information for AI coding agents and developers working on the rflasher codebase.

## Project Overview

rflasher is a modern Rust implementation for reading, writing, and erasing SPI flash chips. It's a workspace-based project with a `no_std` compatible core and dual-mode support (sync/async) using the `maybe-async` crate.

**Language**: Rust 2021 Edition  
**Architecture**: Multi-crate workspace with 18+ crates under `crates/`

## Build Commands

### Standard Build
```bash
# Build all default crates (excludes WASM)
cargo build --release

# Build with all features (requires libftdi1-dev)
cargo build --release --all-features

# Build specific programmer only
cargo build --release --no-default-features --features ch341a,serprog

# Build specific crate
cargo build -p rflasher-core --release

# Debug build
cargo build
```

### WASM Build (Special)
```bash
# One-time setup
rustup target add wasm32-unknown-unknown
cargo install trunk

# Build WASM interface
cd crates/rflasher-wasm
RUSTFLAGS="--cfg=web_sys_unstable_apis" trunk build --release

# Development server with hot-reload
trunk serve
```

## Test Commands

### Run All Tests
```bash
# Run all tests (excludes WASM crate)
cargo test --workspace --exclude rflasher-wasm --all-features

# Run tests for specific crate
cargo test -p rflasher-core

# Run single test by name
cargo test test_jedec_id_parsing

# Run tests in a specific file (use module path)
cargo test -p rflasher-core chip::database::tests

# Run with output shown
cargo test -- --nocapture

# Run tests matching pattern
cargo test erase --all-features
```

### Test Organization
- Tests are in `#[cfg(test)] mod tests {}` blocks within source files
- No separate `tests/` directories currently
- Use `#[test]` attribute for test functions

## Lint and Format Commands

### Format Check
```bash
# Check formatting (CI mode)
cargo fmt --all -- --check

# Auto-format all code
cargo fmt --all
```

### Clippy Linting
```bash
# Run clippy with strict warnings (CI mode)
cargo clippy --workspace --exclude rflasher-wasm --all-targets --all-features -- -D warnings

# Run clippy for single crate
cargo clippy -p rflasher-core -- -D warnings

# Run clippy for WASM
cargo clippy --package rflasher-wasm --target wasm32-unknown-unknown -- -D warnings

# Auto-fix clippy suggestions (when possible)
cargo clippy --fix --allow-dirty
```

### System Dependencies
Some features require system libraries:
```bash
# Ubuntu/Debian
sudo apt-get install libudev-dev libftdi1-dev

# Fedora
sudo dnf install libudev-devel libftdi-devel

# Arch
sudo pacman -S libftdi
```

## Code Style Guidelines

### General Principles (from .opencode/agent/rust-functional-reviewer.md)

1. **Functional Over Imperative**: Prefer iterators (`map`, `filter`, `fold`, `collect`, `flat_map`) over explicit `for`/`while` loops
2. **Zero-Cost Abstractions**: Leverage Rust's move semantics and avoid unnecessary overhead
3. **Safety First**: Always prefer proper error handling

### Imports Organization
```rust
// 1. Standard library / core imports
#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::fmt;

// 2. External crate imports (alphabetically)
use nusb::{Device, Interface};
use maybe_async::maybe_async;

// 3. Internal crate imports (grouped logically)
use crate::chip::ChipDatabase;
use crate::error::{Error, Result};
use crate::programmer::SpiMaster;

// 4. Re-exports
pub use device::Ch341a;
pub use error::Result;
```

### Error Handling
```rust
// GOOD: Use Result and ? operator
fn probe_chip<M: SpiMaster>(master: &mut M) -> Result<FlashContext> {
    let jedec = protocol::read_jedec_id(master)?;
    find_chip_by_jedec(jedec)
}

// GOOD: Pattern matching when needed
match result {
    Ok(value) => process(value),
    Err(Error::ChipNotFound) => fallback(),
    Err(e) => return Err(e),
}

// BAD: Avoid unwrap/expect except in tests
let value = result.unwrap(); // Don't do this!
let value = result.expect("must work"); // Only in tests or examples
```

### Type Definitions
```rust
// Custom error types with Display impl
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    ChipNotFound,
    SpiTimeout,
    WriteProtected,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChipNotFound => write!(f, "flash chip not found"),
            // ...
        }
    }
}

// Use type aliases for common patterns
pub type Result<T> = core::result::Result<T, Error>;
```

### Functional Patterns
```rust
// GOOD: Iterator chains
let result: Vec<_> = data
    .iter()
    .filter(|&x| x > 0)
    .map(|x| x * 2)
    .collect();

// GOOD: any/all for boolean checks
let needs_erase = have.iter()
    .zip(want.iter())
    .any(|(h, w)| (h & w) != *w);

// BAD: Imperative loops (unless necessary for performance/clarity)
let mut result = Vec::new();
for item in data {
    if item > 0 {
        result.push(item * 2);
    }
}
```

### Naming Conventions
- **Types**: `PascalCase` - `FlashContext`, `SpiMaster`, `ChipDatabase`
- **Functions/methods**: `snake_case` - `read_jedec_id`, `execute_command`, `find_chip`
- **Constants**: `SCREAMING_SNAKE_CASE` - `ERASED_VALUE`, `DEFAULT_TIMEOUT`
- **Modules**: `snake_case` - `chip`, `programmer`, `flash`
- **Features**: `kebab-case` - `is_sync`, `all-programmers`

### Documentation
```rust
//! Module-level docs at the top of files
//!
//! Use `//!` for crate/module documentation

/// Function documentation using `///`
///
/// # Arguments
/// * `master` - The SPI master to use
/// * `addr` - Starting address
///
/// # Returns
/// The number of bytes read, or an error
///
/// # Errors
/// Returns `Error::SpiTimeout` if operation times out
pub fn read_data<M: SpiMaster>(master: &mut M, addr: u32) -> Result<usize> {
    // Implementation
}
```

### Feature Gates
```rust
// no_std compatibility
#![no_std]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]
#![allow(async_fn_in_trait)] // For maybe-async support

#[cfg(feature = "std")]
extern crate std;

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

// Conditional compilation for sync/async
#[maybe_async]
pub trait SpiMaster {
    async fn execute(&mut self, cmd: &mut SpiCommand) -> Result<()>;
}
```

### Async/Sync Dual Mode
- Core library supports both sync and async via `maybe-async` crate
- **Sync mode**: Enabled with `is_sync` feature (for CLI)
- **Async mode**: Default (for WASM/browser)
- Both modes compile from the same source code

## Common Pitfalls

1. **WASM builds require special flags**: `RUSTFLAGS="--cfg=web_sys_unstable_apis"`
2. **Test the workspace**: Always use `--workspace --exclude rflasher-wasm`
3. **Feature dependencies**: Some crates require `is_sync` feature for sync mode
4. **no_std constraints**: Core crate must remain `no_std` compatible
5. **Clippy warnings as errors**: CI treats warnings as errors (`-D warnings`)

## CI Configuration

The project uses GitHub Actions (`.github/workflows/ci.yml`) with 4 jobs:
- **fmt**: Checks code formatting with `cargo fmt`
- **clippy**: Lints with clippy treating warnings as errors
- **test**: Runs all tests with all features
- **build**: Release build with all features
- **wasm**: Separate WASM build and clippy check

All CI jobs (except WASM) exclude the `rflasher-wasm` crate and use `--all-features`.

## Workspace Structure

```
rflasher/
├── Cargo.toml                 # Workspace definition
├── src/                       # Main binary crate
├── crates/
│   ├── rflasher-core/        # no_std core library (chip DB, protocols)
│   ├── rflasher-flash/       # Unified flash device abstraction
│   ├── rflasher-chips-codegen/ # Build-time chip DB generator
│   ├── rflasher-wasm/        # Browser WASM interface
│   ├── rflasher-ch341a/      # CH341A programmer
│   ├── rflasher-ch347/       # CH347 programmer
│   ├── rflasher-serprog/     # Serprog protocol
│   ├── rflasher-ftdi/        # FTDI programmers
│   └── ... (14+ more programmer crates)
├── chips/vendors/            # RON chip definitions
└── .github/workflows/        # CI configuration
```

## Review Checklist

Before committing code:
- [ ] Run `cargo fmt --all`
- [ ] Run `cargo clippy --workspace --exclude rflasher-wasm --all-features -- -D warnings`
- [ ] Run `cargo test --workspace --exclude rflasher-wasm --all-features`
- [ ] Check for imperative loops that could be iterators
- [ ] Ensure error handling uses `Result` and `?` (no unwrap/expect)
- [ ] Verify no_std compatibility if touching core crate
- [ ] Add documentation for public APIs
- [ ] Update tests if changing behavior

## References

- Main documentation: `README.md`
- Code review agent: `.opencode/agent/rust-functional-reviewer.md`
- CI configuration: `.github/workflows/ci.yml`
- Upstream project: [flashprog](https://github.com/SourceArcade/flashprog)
