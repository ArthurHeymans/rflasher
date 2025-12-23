# rflasher

A modern Rust implementation for reading, writing, and erasing SPI flash chips. This is a loose port of [flashprog](https://github.com/SourceArcade/flashprog).

## Features

- **Modern Rust Architecture**: Clean separation of concerns with workspace organization
- **`no_std` Compatible Core**: Designed for potential embedded and async use cases. WIP
- **RON-based Chip Database**: Human-readable chip definitions with build-time code generation
- **Trait-based Programmer Abstraction**: Extensible design for adding new programmers
- **Layout Support**: Intel Flash Descriptor (IFD) and FMAP parsing for region-based operations
- **Progress Reporting**: Real-time progress bars for all operations using `indicatif`
- **Safety Features**: Write protection detection, verification, and region-based access control

## Supported Programmers

Currently, rflasher supports **SPI-based programmers only**:

- **CH341A** - USB SPI programmer (VID: 0x1A86, PID: 0x5512)
- **Serprog** - Serial Flasher Protocol (serial port and TCP/IP)
- **FTDI** - MPSSE-based programmers (FT2232H, FT4232H, FT232H, and compatible devices)
- **Linux SPI** - Native Linux spidev interface
- **Dummy** - In-memory flash emulator for testing

## Supported Flash Chips

Currently includes **57 flash chips** from:

- **Winbond** - W25Q and W25X series (24 chips)
- **GigaDevice** - GD25Q and GD25LQ series (15 chips)
- **Macronix** - MX25L, MX25U, MX25R, and MX66 series (18 chips)

Chip definitions are stored as RON files in `chips/vendors/` and can be easily extended. See the [chip database structure](chips/vendors/) for examples.

## Installation

### Build from Source

```bash
# Clone the repository
git clone https://github.com/user/rflasher
cd rflasher

# Build with default features (all programmers except FTDI)
cargo build --release

# Build with all programmers (requires libftdi1-dev)
cargo build --release --features all-programmers

# Build with specific programmers only
cargo build --release --no-default-features --features ch341a,serprog

# Install to ~/.cargo/bin
cargo install --path .
```

### System Requirements

- **Rust toolchain** 1.70 or later
- **libftdi1** (optional, for FTDI programmer support)
  - Debian/Ubuntu: `sudo apt install libftdi1-dev`
  - Fedora: `sudo dnf install libftdi-devel`
  - Arch: `sudo pacman -S libftdi`

### Chip Database

The flash chip database is loaded from RON files at runtime. Default search paths:

1. `./chips/vendors/` (local development)
2. `/usr/share/rflasher/chips/` (system-wide installation)
3. `/usr/local/share/rflasher/chips/` (local installation)

You can also specify a custom path with `--chip-db <path>`.

### USB Device Permissions

For CH341A and FTDI programmers, you may need to set up udev rules:

```bash
# Copy udev rules (create this file based on your needs)
sudo cp 50-rflasher.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger
```

Example udev rule for CH341A:
```
# CH341A USB programmer
SUBSYSTEM=="usb", ATTR{idVendor}=="1a86", ATTR{idProduct}=="5512", MODE="0666"
```

### Man Page

A man page is available in the `man/` directory. To view it locally:

```bash
man -l man/rflasher.1
```

To install system-wide:

```bash
sudo cp man/rflasher.1 /usr/local/share/man/man1/
sudo mandb
```

The man page is automatically generated from the CLI definitions using `clap_mangen`. To regenerate it:

```bash
cargo run --bin gen-manpage
```

## Quick Start

```bash
# List available programmers
rflasher list-programmers

# List supported chips
rflasher list-chips

# Probe for a flash chip using CH341A
rflasher probe -p ch341a

# Show detailed chip information
rflasher info -p ch341a

# Read flash to a file
rflasher read -p ch341a -o flash_backup.bin

# Write a file to flash (with automatic erase and verify)
rflasher write -p ch341a -i firmware.bin

# Erase entire chip
rflasher erase -p ch341a
```

## Usage Examples

### Basic Operations

```bash
# Read entire flash chip
rflasher read -p ch341a -o backup.bin

# Write and verify (default behavior)
rflasher write -p ch341a -i firmware.bin

# Write without verification (faster, but risky)
rflasher write -p ch341a -i firmware.bin --verify=false

# Verify flash contents against a file
rflasher verify -p ch341a -i firmware.bin

# Erase specific region (64 KiB starting at 0x10000)
rflasher erase -p ch341a --start 0x10000 --length 0x10000
```

### Programmer-Specific Options

```bash
# CH341A (USB)
rflasher probe -p ch341a

# Serprog via serial port
rflasher probe -p serprog:dev=/dev/ttyUSB0

# Serprog via serial with custom baud rate
rflasher probe -p serprog:dev=/dev/ttyUSB0:115200

# Serprog via TCP (e.g., ESP8266-based programmer)
rflasher probe -p serprog:ip=192.168.1.100:5000

# FTDI with specific device type
rflasher probe -p ftdi:type=2232h

# FTDI on channel B with slower clock
rflasher probe -p ftdi:type=2232h,port=B,divisor=10

# Linux SPI with custom speed
rflasher probe -p linux_spi:dev=/dev/spidev0.0,spispeed=4000
```

### Layout Operations

Flash layouts allow you to work with specific regions of the flash chip (e.g., BIOS, ME, GbE regions on Intel systems).

```bash
# Extract Intel Flash Descriptor from a flash image
rflasher layout ifd -i flash.bin -o layout.toml

# Extract FMAP from a Chromebook flash image
rflasher layout fmap -i chromebook.bin -o layout.toml

# Show layout from a file
rflasher layout show -f layout.toml

# Create a new layout template
rflasher layout create -o custom.toml --size "16 MiB"

# Read only the BIOS region (using IFD from chip)
rflasher read -p ch341a --ifd --region bios -o bios.bin

# Write to a specific region from a layout file
rflasher write -p ch341a --layout layout.toml --region bios -i bios_update.bin

# Erase multiple regions
rflasher erase -p ch341a --ifd --include bios,descriptor
```

### Verbosity and Debugging

```bash
# Increase verbosity (shows debug messages)
rflasher -v probe -p ch341a

# Maximum verbosity (shows trace-level messages)
rflasher -vv read -p ch341a -o flash.bin
```

## Architecture

rflasher uses a workspace structure with clear separation of concerns:

- **`rflasher-core`** - `no_std` core library with chip database, SPI protocol, and flash operations
- **`rflasher-flash`** - Unified flash device abstraction (works with both SPI and opaque programmers)
- **`rflasher-chips-codegen`** - Build-time code generator for chip database
- **`rflasher-ch341a`** - CH341A USB programmer support
- **`rflasher-serprog`** - Serial Flasher Protocol implementation
- **`rflasher-ftdi`** - FTDI MPSSE programmer support
- **`rflasher-linux-spi`** - Linux spidev interface
- **`rflasher-dummy`** - In-memory flash emulator for testing

See [ARCHITECTURE.md](ARCHITECTURE.md) for detailed implementation information.

## Safety Warnings

⚠️ **IMPORTANT SAFETY INFORMATION**

Flash chip programming can permanently damage your hardware if done incorrectly:

- **Voltage Mismatches**: Ensure your programmer voltage matches the flash chip (typically 3.3V for modern SPI flash)
- **Wrong Chip**: Writing to the wrong chip can brick your device
- **Intel ME Region**: On Intel systems, corrupting the Management Engine region can brick the motherboard
- **Write Protection**: Always check write protection status before writing
- **Backup First**: Always read and backup your flash chip before making any changes

**This software comes with NO WARRANTY. Use at your own risk.**

## Contributing

Contributions are welcome! Here are some ways you can help:

### Adding New Flash Chips

Flash chips are defined in RON files under `chips/vendors/`. To add a new chip:

1. Find the datasheet for your chip
2. Create or update the vendor file (e.g., `chips/vendors/winbond.ron`)
3. Add chip definition with JEDEC ID, size, erase blocks, and features
4. Submit a pull request

Example chip definition:

```ron
(
    name: "W25Q128.V",
    device_id: 0x4018,
    total_size: MiB(16),
    features: (
        wrsr_wren: true,
        fast_read: true,
        quad_io: true,
        erase_4k: true,
        erase_64k: true,
    ),
    voltage: (min: 2700, max: 3600),
    erase_blocks: [
        (opcode: 0x20, size: KiB(4)),
        (opcode: 0xD8, size: KiB(64)),
        (opcode: 0xC7, size: MiB(16)),
    ],
    tested: (probe: Ok, read: Ok, erase: Ok, write: Ok, wp: Ok),
)
```

### Adding New Programmers

To add a new SPI programmer:

1. Create a new crate in `crates/rflasher-yourprogrammer/`
2. Implement the `SpiMaster` trait from `rflasher-core`
3. Add programmer registration in `rflasher-flash`
4. Update documentation and feature flags

See existing programmer crates for examples.

## TODO

The following features are planned for future development:

- [ ] **Port more flash chips** - The original flashprog has ~600 chip definitions; we currently have 57
- [ ] **Add more SPI programmers** - Port remaining SPI programmers from flashprog
- [ ] **Intel Internal Programmer** - Support for reading/writing via Intel chipset (OpaqueMaster)
- [ ] **Optimal erase algorithm** - Minimize erase operations by using largest possible erase blocks
- [ ] **SFDP parsing** - Auto-detection of chip parameters via Serial Flash Discoverable Parameters
- [ ] **Write protection management** - Full write protection range management and status reporting

## License

This project is licensed under the **GNU General Public License v2.0 or later** (GPL-2.0-or-later), the same license as flashprog.

See [LICENSE](LICENSE) for the full license text.

## Acknowledgments

This project is a loose port of [flashprog](https://github.com/SourceArcade/flashprog), which itself is a fork of flashrom. Thanks to all the contributors of those projects for their extensive work on flash chip support and programmer implementations.

## Related Projects

- **flashprog** - https://github.com/SourceArcade/flashprog - The upstream C implementation
- **flashrom** - https://www.flashrom.org/ - The original flash chip programmer

---

**Note**: rflasher is currently focused on **SPI flash chips only**. Parallel flash and other protocols are not currently in scope, though the architecture supports future OpaqueMaster implementations for such devices.
