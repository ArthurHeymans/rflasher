# rflasher

A modern Rust implementation for reading, writing, and erasing SPI flash chips. This is a loose port of [flashprog](https://github.com/SourceArcade/flashprog).

> **⚠️ ALPHA SOFTWARE WARNING**
>
> rflasher is currently in **alpha stage** and should **not be relied upon for production use**. For critical flash programming tasks, please use the original [flashprog](https://github.com/SourceArcade/flashprog) instead. This project is under active development and may contain bugs that could damage your hardware or data.

## Features

- **Modern Rust Architecture**: Clean separation of concerns with workspace organization
- **Dual-Mode Support**: Synchronous CLI and asynchronous WASM/browser support from a single codebase
- **Web Interface**: Browser-based UI using egui and WebSerial API for programming flash chips directly in Chrome/Edge
- **`no_std` Compatible Core**: Designed for potential embedded and async use cases. WIP
- **RON-based Chip Database**: Human-readable chip definitions with build-time code generation
- **Trait-based Programmer Abstraction**: Extensible design for adding new programmers
- **Layout Support**: Intel Flash Descriptor (IFD) and FMAP parsing for region-based operations
- **Progress Reporting**: Real-time progress bars for all operations using `indicatif`
- **Safety Features**: Write protection detection, verification, and region-based access control
- **Experimental REPL**: Steel Scheme-based REPL for scripting raw SPI commands (requires `--features repl`)

## Supported Programmers

Currently, rflasher supports the following programmers:

### SPI-based Programmers

- **CH341A** - USB SPI programmer (VID: 0x1A86, PID: 0x5512)
- **CH347** - USB SPI programmer (VID: 0x1A86, PID: 0x55DB/0x55DE)
- **Dediprog** - Professional USB SPI programmers (SF100, SF200, SF600, SF600PG2, SF700)
- **Serprog** - Serial Flasher Protocol (serial port and TCP/IP)
- **FTDI** - MPSSE-based programmers (FT2232H, FT4232H, FT232H, and compatible devices)
- **FT4222H** - FTDI FT4222H USB to SPI bridge (VID: 0x0403, PID: 0x601C)
- **Raiden** - Chrome OS debug hardware (SuzyQable, Servo V4, C2D2, uServo, Servo Micro)
- **Internal** - Built-in chipset SPI controllers (Intel ICH7-500 Series, AMD FCH 790b)
- **Linux SPI** - Native Linux spidev interface (`/dev/spidevX.Y`)
- **Linux GPIO** - GPIO bitbang SPI via Linux character device (`/dev/gpiochipN`)
- **Dummy** - In-memory flash emulator for testing

### Opaque Programmers

- **Linux MTD** - Linux Memory Technology Device interface (`/dev/mtdN`) for NOR flash

## Supported Flash Chips

Currently includes **482 flash chips** from:

- **AMIC** - A25L and A25LQ series (21 chips)
- **Atmel** - AT25DF and AT26DF series (31 chips)
- **Boya** - BY25Q series (10 chips)
- **Eon** - EN25F, EN25Q, EN25QH, EN25B, EN25P, and EN25S series (47 chips)
- **ESI** - ES25P series (3 chips)
- **ESMT** - F25L series (7 chips)
- **Fudan** - FM25F and FM25Q series (12 chips)
- **GigaDevice** - GD25Q, GD25LQ, GD25WQ, GD25VQ, GD25B, GD25LB, GD25LE, and GD25LR series (61 chips)
- **Intel** - 25F series (6 chips)
- **ISSI** - IS25LP and IS25WP series (10 chips)
- **Macronix** - MX25L, MX25U, MX25R, and MX66 series (53 chips)
- **Micron/Numonyx** - N25Q, MT25Q, MT25QL, MT25QU, and M25P series (55 chips)
- **Nantronics** - N25S series (5 chips)
- **PMC** - Pm25L series (17 chips)
- **Puya** - P25Q and PY25Q/PY25F/PY25R series (30 chips)
- **Sanyo** - LE25FU and LE25FW series (12 chips)
- **Spansion** - S25FL series (27 chips)
- **SST** - SST25VF, SST25LF, SST25WF, and SST26VF series (34 chips)
- **Winbond** - W25Q, W25X, W25P, and W25R series (54 chips)
- **XMC** - XM25QH and XM25QU series (6 chips)
- **XTX** - XT25F series (11 chips)
- **Zetta** - ZD25D and ZD25LQ series (4 chips)

Chip definitions are stored as RON files in `chips/vendors/` and can be easily extended. See the [chip database structure](chips/vendors/) for examples.

## Installation

### Build from Source

```bash
# Clone the repository
git clone https://github.com/user/rflasher
cd rflasher

# Build with default features (most common programmers)
cargo build --release

# Build with all programmers (includes FTDI, requires libftdi1-dev)
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

# Dediprog SF600 with 12MHz SPI speed
rflasher probe -p dediprog:spispeed=12M

# Raiden Debug SPI (Chrome OS debug hardware)
rflasher probe -p raiden

# Internal chipset programmer (Intel/AMD)
rflasher probe -p internal

# FTDI with specific device type
rflasher probe -p ftdi:type=2232h

# FTDI on channel B with slower clock
rflasher probe -p ftdi:type=2232h,port=B,divisor=10

# FT4222H with custom speed and chip select
rflasher probe -p ft4222:spispeed=20000,cs=0

# Linux SPI with custom speed
rflasher probe -p linux_spi:dev=/dev/spidev0.0,spispeed=4000

# Linux GPIO bitbang SPI (e.g., Raspberry Pi)
rflasher probe -p linux_gpio_spi:gpiochip=0,cs=25,sck=11,mosi=10,miso=9

# Linux GPIO with custom speed
rflasher read -p linux_gpio_spi:dev=/dev/gpiochip0,cs=25,sck=11,mosi=10,miso=9,spispeed=500 -o flash.bin

# Linux MTD device
rflasher probe -p linux_mtd:dev=0

# Linux MTD - read from device 0
rflasher read -p linux_mtd:dev=0 -o flash_backup.bin
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

### Write Protection Operations

rflasher supports reading and modifying flash chip write protection settings:

```bash
# Show current write protection status
rflasher wp status -p ch341a

# List all available protection ranges for the chip
rflasher wp list -p ch341a

# Enable hardware write protection (WP# pin controlled)
rflasher wp enable -p ch341a

# Disable write protection
rflasher wp disable -p ch341a

# Set protection for a specific address range (start,length)
rflasher wp range -p ch341a 0,0x100000

# Set protection for a named region (requires layout)
rflasher wp region -p ch341a --ifd bios

# Make changes temporary (volatile, lost on power cycle)
rflasher wp enable -p ch341a --temporary
```

### Verbosity and Debugging

```bash
# Increase verbosity (shows debug messages)
rflasher -v probe -p ch341a

# Maximum verbosity (shows trace-level messages)
rflasher -vv read -p ch341a -o flash.bin
```

### Experimental: Scheme REPL

> **Note**: The REPL is an experimental feature and requires building with `--features repl`.

rflasher includes a Steel Scheme-based REPL for scripting raw SPI commands. This is useful for advanced users who need to execute custom SPI sequences, experiment with flash commands, or automate testing.

```bash
# Build with REPL support
cargo build --release --features repl

# Start REPL with serprog programmer
rflasher repl -p serprog:dev=/dev/ttyACM0

# Start REPL with CH341A
rflasher repl -p ch341a
```

Example REPL session:

```scheme
        __ _           _
   _ _ / _| |__ _ _____| |_  ___ _ _
  | '_|  _| / _` (_-< ' \/ -_) '_|    Version 0.1.0
  |_| |_| |_\__,_/__/_||_\___|_|      :? for help

Type (rflasher-help) for available commands, (quit) or (exit) to exit.

λ > (read-jedec-id)
=> (239 16389)

λ > (read-status1)
=> 0

λ > (spi-read READ 0 16)
=> (255 255 255 255 255 255 255 255 255 255 255 255 255 255 255 255)

λ > (bytes->hex (spi-read READ 0 32))
=> "ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff"

λ > (define data (make-bytes 256 #xAA))
λ > (write-enable)
=> #t
λ > (spi-write PP #x1000 data)
=> #t
λ > (wait-ready)
=> #t

λ > (quit)
Goodbye!
```

Available functions include:
- **SPI operations**: `spi-transfer`, `spi-read`, `spi-write`, `read-jedec-id`, `read-status1/2/3`, `write-enable`, `write-disable`, `is-busy?`, `wait-ready`
- **Erase operations**: `chip-erase`, `sector-erase`, `block-erase-32k`, `block-erase-64k`
- **Byte utilities**: `make-bytes`, `bytes-length`, `bytes-ref`, `bytes-set!`, `bytes->list`, `list->bytes`, `bytes->hex`, `hex->bytes`, `bytes-slice`
- **SPI25 constants**: `WREN`, `WRDI`, `RDSR`, `WRSR`, `READ`, `FAST_READ`, `PP`, `SE`, `BE_32K`, `BE_64K`, `CE`, `RDID`, etc.

Type `(rflasher-help)` in the REPL for the full list of commands.

## Web Interface (WASM)

rflasher includes a browser-based web interface that allows you to program flash chips directly from your web browser using the WebSerial API. This is useful for scenarios where installing native software is difficult or when you need a portable, cross-platform solution.

### Features

- **Browser-based UI**: Modern egui-based interface running entirely in the browser
- **WebSerial Support**: Connect to serprog programmers via USB serial ports
- **Full Flash Operations**: Read, write, erase, verify, and probe flash chips
- **Progress Reporting**: Real-time progress updates for all operations
- **File Handling**: Load firmware files and save flash dumps directly in the browser
- **No Installation Required**: Run directly in a compatible web browser

### Browser Requirements

The web interface requires a browser with WebSerial API support:

- **Chrome/Edge**: Version 89+ (full support)
- **Opera**: Version 75+ (full support)
- **Firefox**: Not yet supported (WebSerial behind flag)
- **Safari**: Not yet supported

**Note**: WebSerial is still an experimental API. Ensure your browser has the necessary permissions to access serial ports.

### Building the Web Interface

The web interface is built using [Trunk](https://trunkrs.dev/), a WASM web application bundler.

```bash
# Install trunk (if not already installed)
cargo install trunk

# Add the wasm32 target
rustup target add wasm32-unknown-unknown

# Build the web interface
cd crates/rflasher-wasm
trunk build --release

# The output will be in the dist/ directory
```

For development with auto-reload:

```bash
cd crates/rflasher-wasm
trunk serve
# Open http://localhost:8080 in your browser
```

### Running Locally

After building, you can serve the web interface locally:

```bash
# Using Python's built-in HTTP server
cd crates/rflasher-wasm/dist
python3 -m http.server 8080

# Using any other static file server
# cd crates/rflasher-wasm/dist
# npx serve
```

Then open `http://localhost:8080` in a compatible browser.

### Deploying to Production

To deploy the web interface to a web server:

1. Build the release version:
   ```bash
   cd crates/rflasher-wasm
   trunk build --release
   ```

2. Copy the contents of `crates/rflasher-wasm/dist/` to your web server:
   ```bash
   rsync -av dist/ user@yourserver:/var/www/html/rflasher/
   ```

3. Ensure your web server is configured to:
   - Serve the `index.html` file as the default page
   - Set appropriate MIME types for `.wasm` files
   - Use HTTPS (required for WebSerial API)

**Important**: The WebSerial API requires a secure context (HTTPS). Local development on `localhost` works, but production deployments must use HTTPS.

### Using the Web Interface

1. Open the web interface in your browser
2. Click **"Connect"** to select your serprog programmer from the serial port list
3. Once connected, use the **Probe** button to detect the flash chip
4. Choose an operation:
   - **Read**: Download the current flash contents
   - **Write**: Upload and write a firmware file to flash
   - **Erase**: Erase the entire flash chip
   - **Verify**: Verify flash contents against a file
5. Monitor progress in the status panel

### Nix Development Environment

If you're using the provided Nix flake, the development environment includes all necessary tools:

```bash
# Enter the Nix development shell
nix develop

# The wasm32 target and trunk are already available
cd crates/rflasher-wasm
trunk serve
```

### Troubleshooting

**"Serial port not found" or "WebSerial not supported"**
- Ensure you're using a compatible browser (Chrome/Edge 89+)
- Check that WebSerial is enabled in your browser settings
- Try accessing via `chrome://flags` and enable "Experimental Web Platform features"

**"Failed to open port"**
- Ensure no other application is using the serial port
- Check USB cable and connections
- Verify the serprog device is properly configured

**Reads hang or timeout**
- This is a known issue being investigated (see transport.rs TODO)
- Try using a different USB cable or port
- Reduce the amount of data being read at once

## Architecture

rflasher uses a workspace structure with clear separation of concerns:

- **`rflasher-core`** - `no_std` core library with chip database, SPI protocol, and flash operations (supports both sync and async via `maybe-async`)
- **`rflasher-flash`** - Unified flash device abstraction (works with both SPI and opaque programmers)
- **`rflasher-chips-codegen`** - Build-time code generator for chip database
- **`rflasher-wasm`** - Browser-based web interface using egui and WebSerial API (async mode)
- **`rflasher-ch341a`** - CH341A USB programmer support
- **`rflasher-ch347`** - CH347 USB programmer support
- **`rflasher-dediprog`** - Dediprog SF-series USB programmer support
- **`rflasher-serprog`** - Serial Flasher Protocol implementation (supports both sync and async)
- **`rflasher-ftdi`** - FTDI MPSSE programmer support
- **`rflasher-ft4222`** - FTDI FT4222H USB to SPI bridge support
- **`rflasher-raiden`** - Raiden Debug SPI (Chrome OS debug hardware) support
- **`rflasher-internal`** - Internal chipset SPI controller support (Intel ICH/PCH, AMD FCH)
- **`rflasher-linux-spi`** - Linux spidev interface
- **`rflasher-linux-gpio`** - Linux GPIO bitbang SPI via character device
- **`rflasher-linux-mtd`** - Linux MTD (Memory Technology Device) interface
- **`rflasher-dummy`** - In-memory flash emulator for testing

### Async/Sync Architecture

The core library uses the [`maybe-async`](https://crates.io/crates/maybe-async) crate to support both synchronous (for CLI/native applications) and asynchronous (for WASM/browser applications) operation from a single codebase:

- **Sync mode** (CLI): Enabled with the `is_sync` feature flag, compiles to blocking synchronous code
- **Async mode** (WASM): Default mode, uses async/await for non-blocking browser operations

This design allows the same flash operations, chip database, and programmer traits to work seamlessly in both native CLI applications and browser-based WASM environments without code duplication.

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

- [x] **Port more flash chips** - Ported 482 out of ~495 SPI flash chips from flashprog (97% coverage)
- [ ] **Add more SPI programmers** - Port remaining SPI programmers from flashprog
- [x] **Intel/AMD Internal Programmer** - Support for reading/writing via chipset SPI controllers
- [x] **Optimal erase algorithm** - Minimize erase operations by using largest possible erase blocks

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
