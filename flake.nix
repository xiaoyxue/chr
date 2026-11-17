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
          # chr-server talks to its workers over GStreamer unixfdsink/unixfdsrc
          # (gst-plugins-bad); the runtime links GStreamer core, app and allocators.
          gstreamer = [
            pkgs.gst_all_1.gstreamer
            pkgs.gst_all_1.gst-plugins-base
          ];
          # livesync (and fallbackswitch, for comparison) of chr-server's GStreamer core
          # (#154), copied out of gst-plugins-rs: the whole package (~55 plugins) would put
          # gtk4, whisper and more on every registry scan; these two need only GStreamer.
          liveSync = pkgs.runCommand "chr-gst-livesync" { } ''
            mkdir -p $out/lib/gstreamer-1.0
            for plugin in livesync fallbackswitch; do
              cp ${pkgs.gst_all_1.gst-plugins-rs}/lib/gstreamer-1.0/libgst$plugin.so $out/lib/gstreamer-1.0/
            done
          '';
          workerPlugins = pkgs.lib.makeSearchPath "lib/gstreamer-1.0" [
            (pkgs.lib.getLib pkgs.gst_all_1.gstreamer)
            pkgs.gst_all_1.gst-plugins-base
            pkgs.gst_all_1.gst-plugins-bad
            liveSync
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
              liveSync
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
            "monitor"
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
            buildInputs = runtime ++ gstreamer;
            # The worker transport tests open unixfd sockets.
            GST_PLUGIN_SYSTEM_PATH_1_0 = workerPlugins;
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
              pname = "chr-runtime";
              cargoExtraArgs = "-p chr-runtime";
              doCheck = false;
              nativeBuildInputs = common.nativeBuildInputs ++ [ pkgs.makeWrapper ];
              postInstall = ''
                for program in chr-server chr-renderer-cli; do
                  wrapProgram $out/bin/$program --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath runtime} \
                    --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : ${workerPlugins}
                done
                for program in chr-tracking-sim-cli chr-sdi-in; do
                  wrapProgram $out/bin/$program --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : ${workerPlugins}
                done
              '';
            }
          );
          monitor = craneLib.buildPackage (
            common
            // {
              inherit cargoArtifacts;
              pname = "chr-monitor";
              cargoExtraArgs = "-p chr-monitor";
              doCheck = false;
            }
          );
          # Static site plus a `vite preview` launcher that also serves the Fusion read-only proxy.
          # The launcher reads FUSION_API_URL/FUSION_API_KEY and `.env*` from the working directory
          # at runtime, so no secret reaches the store.
          pitchStudio = pkgs.buildNpmPackage rec {
            pname = "pitch-studio";
            inherit ((pkgs.lib.importJSON ./client/pitch-studio/package.json)) version;
            src = pkgs.lib.fileset.toSource {
              root = ./client/pitch-studio;
              fileset = pkgs.lib.fileset.unions [
                ./client/pitch-studio/index.html
                ./client/pitch-studio/package.json
                ./client/pitch-studio/package-lock.json
                ./client/pitch-studio/tsconfig.json
                ./client/pitch-studio/vite.config.ts
                ./client/pitch-studio/scripts
                ./client/pitch-studio/src
                ./client/pitch-studio/tests
              ];
            };
            npmDeps = pkgs.importNpmLock { npmRoot = src; };
            npmConfigHook = pkgs.importNpmLock.npmConfigHook;
            doCheck = true;
            checkPhase = ''
              runHook preCheck
              npm run test:offline
              runHook postCheck
            '';
            installPhase = ''
              runHook preInstall
              share=$out/share/pitch-studio
              mkdir -p $share
              cp -r dist scripts node_modules package.json vite.config.ts $share/
              # The studio store defaults to output/pitch-studio under the working directory.
              makeWrapper ${pkgs.nodejs}/bin/node $out/bin/pitch-studio \
                --run 'export STUDIO_DATA_DIR="''${STUDIO_DATA_DIR:-$PWD/output/pitch-studio}"' \
                --add-flags "$share/node_modules/vite/bin/vite.js preview" \
                --add-flags "--config $share/vite.config.ts --configLoader runner --outDir $share/dist"
              runHook postInstall
            '';
            meta.mainProgram = "pitch-studio";
          };
          shell = pkgs.mkShell {
            packages = [
              toolchain
              pkgs.git
              pkgs.pkg-config
              pkgs.nixfmt
            ]
            ++ clientTools
            ++ runtime
            ++ gstreamer;
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtime;
            GST_PLUGIN_SYSTEM_PATH_1_0 = workerPlugins;
            shellHook = ''
              export CARGO_TARGET_DIR="$(git rev-parse --show-toplevel)/target/linux"
            '';
          };
        in
        {
          inherit
            app
            server
            monitor
            pitchStudio
            shell
            ;
          checks = {
            pitch-studio = pitchStudio;
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
                    -p 'test_*.py' > "$out" 2>&1
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
        chr-monitor = (environment system).monitor;
        pitch-studio = (environment system).pitchStudio;
      });
      apps = perSystem (system: {
        default = {
          type = "app";
          program = "${(environment system).app}/bin/chr-app";
        };
        pitch-studio = {
          type = "app";
          program = "${(environment system).pitchStudio}/bin/pitch-studio";
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
