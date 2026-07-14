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
      nixpkgs,
      rust-overlay,
      flake-utils,
      ...
    }:
    flake-utils.lib.eachSystem
      [
        "x86_64-linux"
        "aarch64-linux"
      ]
      (
        system:
        let
          overlays = [ (import rust-overlay) ];
          pkgs = import nixpkgs {
            inherit system overlays;
          };
          inherit (pkgs) lib;

          cargoLock.lockFile = ./Cargo.lock;

          runtimeBuildInputs = p: [
            p.udev
            p.libftdi1
            p.pciutils
          ];

          packageAttrs = {
            pname = "rflasher";
            version = (lib.importTOML ./Cargo.toml).workspace.package.version;
            # Only what the build needs; keeps target/ and other local
            # artifacts out of the store path and the source hash stable.
            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [
                ./Cargo.toml
                ./Cargo.lock
                ./build.rs
                ./src
                ./crates
              ];
            };

            inherit cargoLock;
            cargoBuildFlags = [
              "--package=rflasher"
              "--bin=rflasher"
            ];
            # Run the whole workspace's tests; the root package has none.
            # rflasher-wasm is excluded (needs the wasm32 target).
            cargoTestFlags = [
              "--workspace"
              "--exclude"
              "rflasher-wasm"
            ];

            postPatch = ''
              substituteInPlace src/main.rs \
                --replace-fail 'PathBuf::from("/usr/share/rflasher/chips"),' \
                  "PathBuf::from(\"$out/share/rflasher/chips\"),"
            '';

            postInstall = ''
              install -Dm644 crates/rflasher-chips/data/vendors/*.ron -t $out/share/rflasher/chips
            '';

            meta = {
              description = "A modern Rust port of flashprog for reading, writing, and erasing flash chips";
              homepage = "https://github.com/ArthurHeymans/rflasher";
              license = lib.licenses.gpl2Plus;
              mainProgram = "rflasher";
              platforms = lib.platforms.linux;
            };
          };

          nativePackage = pkgs.rustPlatform.buildRustPackage (
            packageAttrs
            // {
              buildInputs = runtimeBuildInputs pkgs;
              nativeBuildInputs = [ pkgs.pkg-config ];
            }
          );

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

          crossPkgsFor = lib.mapAttrs (
            _: target:
            import nixpkgs {
              inherit system overlays;
              crossSystem.config = target.config;
            }
          ) crossTargets;

          mkCrossPackage =
            name: target:
            let
              crossPkgs = crossPkgsFor.${name};
              isMusl = target.isMusl or false;
            in
            crossPkgs.rustPlatform.buildRustPackage (
              packageAttrs
              // {
                buildInputs = lib.optionals (!isMusl) (runtimeBuildInputs crossPkgs);
                nativeBuildInputs = [ crossPkgs.buildPackages.pkg-config ];
              }
              // lib.optionalAttrs isMusl {
                buildNoDefaultFeatures = true;
                buildFeatures = [ "static-linux-programmers" ];
              }
            );

          rustToolchain = pkgs.rust-bin.stable.latest.default.override {
            extensions = [
              "rust-src"
              "rust-analyzer"
            ];
            targets = [ "wasm32-unknown-unknown" ];
          };

          rustToolchainCross = pkgs.rust-bin.stable.latest.default.override {
            extensions = [
              "rust-src"
              "rust-analyzer"
            ];
            targets = lib.mapAttrsToList (_: target: target.rustTarget) crossTargets;
          };

          mkCrossDevShell =
            name: target:
            let
              crossPkgs = crossPkgsFor.${name};
              isMusl = target.isMusl or false;
              crossBuildInputs = lib.optionals (!isMusl) (runtimeBuildInputs crossPkgs);
            in
            pkgs.mkShell (
              {
                packages = [
                  pkgs.pkg-config
                  rustToolchainCross
                ];
                buildInputs = crossBuildInputs;

                "${target.cargoLinkerEnv}" = "${crossPkgs.stdenv.cc}/bin/${crossPkgs.stdenv.cc.targetPrefix}cc";

                shellHook = ''
                  echo "rflasher cross-compilation environment (${target.rustTarget})"
                  echo "Build with: cargo build --target ${target.rustTarget}${
                    if isMusl then " --no-default-features --features static-linux-programmers" else ""
                  }"
                '';
              }
              // lib.optionalAttrs (!isMusl) {
                PKG_CONFIG_SYSROOT_DIR = "${crossPkgs.stdenv.cc.libc}";
              }
            );

          crossPackages = lib.mapAttrs' (
            name: target: lib.nameValuePair "cross-${name}" (mkCrossPackage name target)
          ) crossTargets;

          crossDevShells = lib.mapAttrs' (
            name: target: lib.nameValuePair "cross-${name}" (mkCrossDevShell name target)
          ) crossTargets;

          linuxCrossOutputs = lib.optionalAttrs (system == "x86_64-linux") {
            packages = crossPackages;
            devShells = crossDevShells;
          };
        in
        {
          packages = {
            default = nativePackage;
            rflasher = nativePackage;
          }
          // (linuxCrossOutputs.packages or { });

          apps.default = {
            type = "app";
            program = "${nativePackage}/bin/rflasher";
          };

          checks.package = nativePackage;
          formatter = pkgs.nixfmt-tree;

          devShells = {
            default = pkgs.mkShell {
              packages = [
                pkgs.pkg-config
                pkgs.trunk
                rustToolchain
              ];
              buildInputs = runtimeBuildInputs pkgs;
            };
          }
          // (linuxCrossOutputs.devShells or { });
        }
      );
}
