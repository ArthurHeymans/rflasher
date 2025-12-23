{
  description = "rflasher - A modern Rust port of flashprog for reading, writing, and erasing flash chips";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };
        
        # Common build inputs for the project
        buildInputs = with pkgs; [
          # USB support
          # libusb1
          
          # Serial port support (requires udev on Linux)
          udev
          
          # For FTDI programmers (future)
          libftdi1
          
          # For internal programmer (future)
          pciutils
        ];
        
        nativeBuildInputs = with pkgs; [
          pkg-config
          rustToolchain
          cargo
        ];
        
      in
      {
        devShells.default = pkgs.mkShell {
          inherit buildInputs nativeBuildInputs;
          
          # Environment variables for pkg-config to find libraries
          PKG_CONFIG_PATH = pkgs.lib.makeSearchPath "lib/pkgconfig" buildInputs;
          
          # For libudev
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
          
          shellHook = ''
            echo "rflasher development environment"
            echo "Rust version: $(rustc --version)"
            echo ""
            echo "Available commands:"
            echo "  cargo build              - Build the project"
            echo "  cargo test               - Run tests"
            echo "  cargo run -- --help      - Show CLI help"
            echo ""
          '';
        };
        
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "rflasher";
          version = "0.1.0";
          
          src = ./.;
          
          cargoLock = {
            lockFile = ./Cargo.lock;
          };
          
          inherit buildInputs nativeBuildInputs;
          
          meta = with pkgs.lib; {
            description = "A modern Rust port of flashprog for reading, writing, and erasing flash chips";
            homepage = "https://github.com/user/rflasher";
            license = licenses.gpl2Plus;
            maintainers = [ ];
          };
        };
      }
    );
}
