# rflasher-wasm

Browser-based web interface for rflasher. It runs an egui UI in WASM and
talks to serprog programmers over WebSerial and to CH341A, CH347, FTDI,
FT4222H, Dediprog, and Raiden programmers over WebUSB, supporting probe,
read, write, erase, and verify with progress reporting. Firmware files are
loaded and flash dumps saved directly in the browser.

## Browser requirements

WebSerial or WebUSB support is required, depending on the programmer:

- Chrome/Edge or Opera (WebSerial 89+, WebUSB 61+)
- Firefox and Safari: not supported (neither API available)

Both APIs require a secure context: `localhost` works for development, but
any deployment must be served over HTTPS.

## Building

The UI is built with [Trunk](https://trunkrs.dev/):

```bash
cargo install trunk
rustup target add wasm32-unknown-unknown
trunk build --release   # output in dist/
```

For development with auto-reload:

```bash
trunk serve   # http://localhost:8080
```

## Running a release build locally

```bash
cd dist
python3 -m http.server 8080
```

(or any other static file server).

## Nix

The repository's Nix flake dev shell already includes the wasm32 target and
trunk:

```bash
nix develop
trunk serve
```

## Troubleshooting

**"Serial port not found", "device not found", or "WebSerial/WebUSB not supported"**

- Use a compatible browser (Chrome/Edge 89+).
- If needed, enable "Experimental Web Platform features" in `chrome://flags`.

**"Failed to open port"**

- Make sure no other application is using the serial port.
- Check the USB cable and connections.
- Verify the serprog device is properly configured.

**Reads hang or time out**

- This is a known issue being investigated (see the TODO in
  `src/transport.rs`).
- Try a different USB cable or port.
- Reduce the amount of data being read at once.
