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

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      flake-utils,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        lib = pkgs.lib;

        # Cross-compilation targets: name -> { config, rustTarget, cargoLinkerEnv }
        crossTargets = {
          i686 = {
            config = "i686-unknown-linux-gnu";
            rustTarget = "i686-unknown-linux-gnu";
            cargoLinkerEnv = "CARGO_TARGET_I686_UNKNOWN_LINUX_GNU_LINKER";
          };
          aarch64 = {
            config = "aarch64-unknown-linux-gnu";
            rustTarget = "aarch64-unknown-linux-gnu";
            cargoLinkerEnv = "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER";
          };
          armv7 = {
            config = "armv7l-unknown-linux-gnueabihf";
            rustTarget = "armv7-unknown-linux-gnueabihf";
            cargoLinkerEnv = "CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER";
          };
          aarch64-musl = {
            config = "aarch64-unknown-linux-musl";
            rustTarget = "aarch64-unknown-linux-musl";
            cargoLinkerEnv = "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER";
            isMusl = true;
          };
        };

        # Generate cross pkgs for each target
        mkCrossPkgs =
          target:
          let
            basePkgs = import nixpkgs {
              inherit system overlays;
              crossSystem.config = target.config;
            };
          in
          if target.isMusl or false then basePkgs.pkgsStatic else basePkgs;

        # Build inputs as a function of pkgs (musl targets skip C system libs)
        mkBuildInputs =
          p:
          if p.stdenv.hostPlatform.isMusl then
            [ ]
          else
            with p;
            [
              udev
              libftdi1
              pciutils
            ];

        # Base rust toolchain with WASM target for web builds
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
          ];
          targets = [ "wasm32-unknown-unknown" ];
        };

        # Rust toolchain with cross targets
        rustToolchainCross = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
          ];
          targets = lib.mapAttrsToList (_: t: t.rustTarget) crossTargets;
        };

        # Create a cross-compilation dev shell for a given target
        mkCrossDevShell =
          name: target:
          let
            crossPkgs = mkCrossPkgs target;
            crossBuildInputs = mkBuildInputs crossPkgs;
            isMusl = target.isMusl or false;
          in
          pkgs.mkShell (
            {
              buildInputs = crossBuildInputs;
              nativeBuildInputs = [
                pkgs.pkg-config
                rustToolchainCross
              ];

              LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

              "${target.cargoLinkerEnv}" = "${crossPkgs.stdenv.cc}/bin/${crossPkgs.stdenv.cc.targetPrefix}cc";

              shellHook = ''
                echo "rflasher cross-compilation environment (${target.rustTarget})"
                echo "Rust version: $(rustc --version)"
                echo ""
                echo "Build with:"
                ${
                  if isMusl then
                    ''
                      echo "  cargo build --target ${target.rustTarget} --no-default-features --features dummy,ch341a,ch347,dediprog,serprog,ft4222,ftdi-native,linux-spi,linux-mtd,internal,raiden"
                      echo ""
                      echo "Note: This is a musl static build. Uses pure-Rust backends (ftdi-native)"
                      echo "to avoid C library dependencies. Binaries are fully statically linked."
                    ''
                  else
                    ''
                      echo "  cargo build --target ${target.rustTarget}"
                    ''
                }
                echo ""
              '';
            }
            // lib.optionalAttrs (!isMusl) {
              PKG_CONFIG_PATH = lib.makeSearchPath "lib/pkgconfig" crossBuildInputs;
              PKG_CONFIG_SYSROOT_DIR = "${crossPkgs.stdenv.cc.libc}";
            }
            // lib.optionalAttrs isMusl {
              RUSTFLAGS = "-C target-feature=+crt-static";
            }
          );

        # Create a cross-compiled package for a given target
        mkCrossPackage =
          name: target:
          let
            crossPkgs = mkCrossPkgs target;
            isMusl = target.isMusl or false;
          in
          crossPkgs.rustPlatform.buildRustPackage (
            {
              pname = "rflasher";
              version = "0.1.0";
              src = ./.;

              cargoLock.lockFile = ./Cargo.lock;

              buildInputs = mkBuildInputs crossPkgs;
              nativeBuildInputs = [
                pkgs.pkg-config
                rustToolchainCross
              ];

              CARGO_BUILD_TARGET = target.rustTarget;

              meta = with lib; {
                description = "A modern Rust port of flashprog for reading, writing, and erasing flash chips";
                homepage = "https://github.com/user/rflasher";
                license = licenses.gpl2Plus;
              };
            }
            // lib.optionalAttrs isMusl {
              # For musl static builds, use pure-Rust backends to avoid C library dependencies
              buildNoDefaultFeatures = true;
              buildFeatures = [
                "dummy"
                "ch341a"
                "ch347"
                "dediprog"
                "serprog"
                "ft4222"
                "ftdi-native"
                "linux-spi"
                "linux-mtd"
                "internal"
                "raiden"
              ];
              RUSTFLAGS = "-C target-feature=+crt-static";
            }
          );

        # Generate all cross dev shells and packages
        crossDevShells = lib.mapAttrs' (
          name: target: lib.nameValuePair "cross-${name}" (mkCrossDevShell name target)
        ) crossTargets;

        crossPackages = lib.mapAttrs' (
          name: target: lib.nameValuePair "cross-${name}" (mkCrossPackage name target)
        ) crossTargets;

      in
      {
        devShells = {
          default = pkgs.mkShell {
            buildInputs = mkBuildInputs pkgs;
            nativeBuildInputs = [
              pkgs.pkg-config
              rustToolchain
              pkgs.cargo
              pkgs.trunk
            ];

            PKG_CONFIG_PATH = lib.makeSearchPath "lib/pkgconfig" (mkBuildInputs pkgs);
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
              echo "Web build (WASM):"
              echo "  cd crates/rflasher-wasm && trunk serve  - Dev server"
              echo "  cd crates/rflasher-wasm && trunk build  - Production build"
              echo ""
              echo "Cross-compilation shells:"
              echo "  nix develop .#cross-i686         - i686-unknown-linux-gnu"
              echo "  nix develop .#cross-aarch64      - aarch64-unknown-linux-gnu"
              echo "  nix develop .#cross-armv7        - armv7-unknown-linux-gnueabihf"
              echo "  nix develop .#cross-aarch64-musl - aarch64-unknown-linux-musl (static)"
              echo ""
            '';
          };
        }
        // crossDevShells;

        packages = {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "rflasher";
            version = "0.1.0";
            src = ./.;

            cargoLock.lockFile = ./Cargo.lock;

            buildInputs = mkBuildInputs pkgs;
            nativeBuildInputs = [
              pkgs.pkg-config
              rustToolchain
              pkgs.cargo
            ];

            meta = with lib; {
              description = "A modern Rust port of flashprog for reading, writing, and erasing flash chips";
              homepage = "https://github.com/user/rflasher";
              license = licenses.gpl2Plus;
            };
          };
        }
        // crossPackages;
      }
    );
}
