{
  description = "chr TypeScript and Rust development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      crane,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      perSystem = nixpkgs.lib.genAttrs systems;
      environment =
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };
          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./render/rust-toolchain.toml;
          craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;
          clientTools = with pkgs; [
            nodejs
            typescript
          ];
          runtime = with pkgs; [
            vulkan-loader
            libGL
            wayland
            libxkbcommon
            libx11
            libxcursor
            libxi
            libxrandr
          ];
          src = pkgs.lib.cleanSourceWith {
            src = ./render;
            filter =
              path: type:
              !builtins.elem (builtins.baseNameOf path) [
                "reference"
                "output"
                "target"
                ".git"
                ".worktree"
                ".worktrees"
                "result"
              ]
              && !(pkgs.lib.hasPrefix "result-" (builtins.baseNameOf path))
              && (craneLib.filterCargoSources path type || pkgs.lib.hasSuffix ".wgsl" path);
          };
          common = {
            inherit src;
            strictDeps = true;
            nativeBuildInputs = [ pkgs.pkg-config ];
            buildInputs = runtime;
          };
          cargoArtifacts = craneLib.buildDepsOnly common;
          app = craneLib.buildPackage (
            common
            // {
              inherit cargoArtifacts;
              pname = "chr-app";
              cargoExtraArgs = "-p chr-app";
              doCheck = false;
              nativeBuildInputs = common.nativeBuildInputs ++ [ pkgs.makeWrapper ];
              postInstall = "wrapProgram $out/bin/chr-app --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath runtime}";
            }
          );
        in
        {
          inherit app;
          checks = {
            client-toolchain =
              pkgs.runCommand "chr-client-toolchain"
                {
                  nativeBuildInputs = clientTools;
                }
                ''
                  export HOME="$TMPDIR"
                  node --version > "$out"
                  npm --version >> "$out"
                  tsc --version >> "$out"
                '';
            fmt = craneLib.cargoFmt { inherit src; };
            clippy = craneLib.cargoClippy (
              common
              // {
                inherit cargoArtifacts;
                cargoClippyExtraArgs = "--workspace --all-targets -- -D warnings";
              }
            );
            tests = craneLib.cargoTest (
              common
              // {
                inherit cargoArtifacts;
                cargoTestExtraArgs = "--workspace";
              }
            );
          };
          shell = pkgs.mkShell {
            packages = [
              toolchain
              pkgs.git
              pkgs.pkg-config
              pkgs.nixfmt
            ]
            ++ clientTools
            ++ runtime;
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtime;
            shellHook = ''
              export CARGO_TARGET_DIR="$(git rev-parse --show-toplevel)/render/target/linux"
            '';
          };
          formatter = pkgs.nixfmt;
        };
    in
    {
      packages = perSystem (system: {
        default = (environment system).app;
        chr-app = (environment system).app;
      });
      apps = perSystem (system: {
        default = {
          type = "app";
          program = "${(environment system).app}/bin/chr-app";
        };
      });
      checks = perSystem (system: (environment system).checks);
      devShells = perSystem (system: {
        default = (environment system).shell;
      });
      formatter = perSystem (system: (environment system).formatter);
    };
}
