{
  description = "Native wgpu SRT video compositor";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/6cdc7fc76e8bf7fde9fa43a849fcaaa70e230dee";

  outputs =
    { nixpkgs, ... }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
      python = pkgs.python3.withPackages (
        ps: with ps; [
          av
          numpy
          opencv4
          pillow
          pyarrow
          wgpu-py
        ]
      );
      libraries = pkgs.lib.makeLibraryPath [
        pkgs.vulkan-loader
        pkgs.libX11
        pkgs.libXext
        pkgs.libglvnd
      ];
      native = pkgs.rustPlatform.buildRustPackage {
        pname = "chr";
        version = "0.1.0";
        src = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            ./Cargo.toml
            ./Cargo.lock
            ./build.rs
            ./native
            ./src
          ];
        };
        cargoLock.lockFile = ./Cargo.lock;
        nativeBuildInputs = [ pkgs.pkg-config ];
        buildInputs = [
          pkgs.ffmpeg
          pkgs.vulkan-headers
          pkgs.vulkan-loader
        ];
      };
      cargoConfig = pkgs.writeText "chr-cargo-config.toml" ''
        [source.crates-io]
        replace-with = "nix-vendor"
        [source.nix-vendor]
        directory = "${native.cargoDeps}"
        [net]
        offline = true
      '';
      app = pkgs.writeShellApplication {
        name = "chr";
        excludeShellChecks = [ "SC1091" ];
        runtimeInputs = [
          pkgs.ffmpeg
        ];
        text = ''
          export LD_LIBRARY_PATH="${libraries}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
          export WGPU_BACKEND_TYPE="''${WGPU_BACKEND_TYPE:-Vulkan}"
          source ${./nix/host-gpu.sh}
          exec ${native}/bin/chr "$@"
        '';
      };
    in
    {
      packages.${system} = {
        default = app;
        cargo-deps = native.cargoDeps;
      };
      apps.${system}.default = {
        type = "app";
        program = "${app}/bin/chr";
      };
      devShells.${system}.default = pkgs.mkShell {
        packages = [
          python
          pkgs.ffmpeg
          pkgs.ruff
          pkgs.nixfmt
          pkgs.cargo
          pkgs.rustc
          pkgs.rustfmt
          pkgs.clippy
          pkgs.pkg-config
          pkgs.vulkan-headers
        ];
        buildInputs = [
          pkgs.ffmpeg
          pkgs.vulkan-loader
        ];
        LD_LIBRARY_PATH = libraries;
        WGPU_BACKEND_TYPE = "Vulkan";
        shellHook = ''
          export CARGO_HOME="$PWD/.runtime/cargo-home"
          mkdir -p -m 700 "$CARGO_HOME"
          ln -sfn ${cargoConfig} "$CARGO_HOME/config.toml"
          source ${./nix/host-gpu.sh}
        '';
      };
      formatter.${system} = pkgs.writeShellApplication {
        name = "chr-format-nix";
        runtimeInputs = [ pkgs.nixfmt ];
        text = ''
          if [ "$#" -eq 0 ]; then
            exec nixfmt flake.nix
          fi
          exec nixfmt "$@"
        '';
      };
    };
}
