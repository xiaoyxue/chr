{
  description = "chr shared Rust/wgpu renderer";

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
          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;
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
            src = ./.;
            filter =
              path: type:
              builtins.baseNameOf path != "reference"
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
              pkgs.pkg-config
              pkgs.nixfmt
            ]
            ++ runtime;
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtime;
            CARGO_TARGET_DIR = "target/linux";
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
