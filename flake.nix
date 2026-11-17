{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
    crane.url = "github:ipetkov/crane";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, crane }:
    flake-utils.lib.eachDefaultSystem
      (system:
        let
          pkgs = (import nixpkgs) {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };
          inherit (pkgs) lib;
          rust_toolchain =
            p: pkgs.rust-bin.stable.latest;
          craneLib = (crane.mkLib pkgs).overrideToolchain (p: (rust_toolchain p).minimal);
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ ];

          # Rust sources plus the JSON fixtures the tests include.
          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              (craneLib.fileset.commonCargoSources ./.)
              ./examples
            ];
          };
          commonArgs = {
            inherit src nativeBuildInputs buildInputs;
            strictDeps = true;
          };
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;

          linear-webhook = craneLib.buildPackage (commonArgs // {
            inherit cargoArtifacts;
            meta = {
              description = "Forward Linear webhooks to Discord";
              mainProgram = "linear-webhook";
              license = lib.licenses.gpl3Only;
            };
          });
        in
        {
          packages = {
            default = linear-webhook;
            inherit linear-webhook;
          };

          checks = {
            inherit linear-webhook;
            # The minimal toolchain has no clippy.
            clippy = ((crane.mkLib pkgs).overrideToolchain (p: (rust_toolchain p).default)).cargoClippy (commonArgs // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- --deny warnings";
            });
          } // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            nixos = pkgs.testers.runNixOSTest (import ./nix/test.nix { inherit self; });
          };

          devShells.default = pkgs.mkShell {
            inherit buildInputs;
            nativeBuildInputs = nativeBuildInputs ++ [
              ((rust_toolchain pkgs).default.override {
                extensions = [ "rust-src" "rustfmt" "rust-analyzer" "clippy" ];
              })
            ];
          };
        }
      ) // {
      nixosModules = {
        default = self.nixosModules.linear-webhook;
        linear-webhook = import ./nix/module.nix { inherit self; };
      };
    };
}
