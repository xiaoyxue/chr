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
          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;
          clientTools = with pkgs; [
            nodejs
            typescript
          ];
          mediaTools = [
            pkgs.ffmpeg
            (pkgs.python3.withPackages (ps: [ ps.pygobject3 ]))
            pkgs.gst_all_1.gstreamer
            pkgs.gst_all_1.gst-plugins-base
            pkgs.gst_all_1.gst-plugins-bad
            pkgs.gst_all_1.gst-plugins-ugly
          ];
          mediaEnvironment = {
            GI_TYPELIB_PATH = pkgs.lib.makeSearchPath "lib/girepository-1.0" [
              (pkgs.lib.getLib pkgs.gst_all_1.gstreamer)
              pkgs.gst_all_1.gst-plugins-base
            ];
            GST_PLUGIN_SYSTEM_PATH_1_0 = pkgs.lib.makeSearchPath "lib/gstreamer-1.0" [
              (pkgs.lib.getLib pkgs.gst_all_1.gstreamer)
              pkgs.gst_all_1.gst-plugins-base
              pkgs.gst_all_1.gst-plugins-bad
              pkgs.gst_all_1.gst-plugins-ugly
            ];
          };
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
          # The Cargo workspace is at the root; its crates live under these directories.
          rustRoots = [
            "server"
            "common"
          ];
          workspaceFiles = [
            "Cargo.toml"
            "Cargo.lock"
            "rust-toolchain.toml"
          ];
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter =
              path: type:
              let
                relative = pkgs.lib.removePrefix "${toString ./.}/" (toString path);
                topLevel = builtins.head (pkgs.lib.splitString "/" relative);
              in
              (builtins.elem relative workspaceFiles || builtins.elem topLevel rustRoots)
              && !builtins.elem (builtins.baseNameOf path) [
                "reference"
                "output"
                "target"
                ".git"
                ".worktree"
                ".worktrees"
                "result"
                "node_modules"
              ]
              && !(pkgs.lib.hasPrefix "result-" (builtins.baseNameOf path))
              && (
                craneLib.filterCargoSources path type
                || pkgs.lib.hasSuffix ".wgsl" path
                || relative == "server/renderer/src/ltc/ltc.bin"
                || relative == "server/renderer/src/ltc/LICENSE.ltc"
                || relative == "server/renderer/examples/area_light_evaluation/comparison.html"
                || relative == "common/tests/fixtures/scene_frame.json"
                || pkgs.lib.hasPrefix "server/runtime/tests/fixtures/" relative
              );
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
          server = craneLib.buildPackage (
            common
            // {
              inherit cargoArtifacts;
              pname = "chr-server";
              cargoExtraArgs = "-p chr-server";
              doCheck = false;
              nativeBuildInputs = common.nativeBuildInputs ++ [ pkgs.makeWrapper ];
              postInstall = "wrapProgram $out/bin/chr-server --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath runtime}";
            }
          );
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
              export CARGO_TARGET_DIR="$(git rev-parse --show-toplevel)/target/linux"
            '';
          };
        in
        {
          inherit app server shell;
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
            media-source =
              pkgs.runCommand "chr-media-source" (mediaEnvironment // { nativeBuildInputs = mediaTools; })
                ''
                  export HOME="$TMPDIR"
                  export PYTHONDONTWRITEBYTECODE=1
                  python -B -m unittest discover -s ${./server/scripts} \
                    -p test_mp4_gstreamer_source.py > "$out" 2>&1
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
          mediaShell = pkgs.mkShell (
            mediaEnvironment
            // {
              inputsFrom = [ shell ];
              inherit (shell) LD_LIBRARY_PATH;
              packages = mediaTools;
            }
          );
          formatter = pkgs.nixfmt;
        };
    in
    {
      packages = perSystem (system: {
        default = (environment system).app;
        chr-app = (environment system).app;
        chr-server = (environment system).server;
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
        media = (environment system).mediaShell;
      });
      formatter = perSystem (system: (environment system).formatter);
    };
}
