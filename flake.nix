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
      app = pkgs.writeShellApplication {
        name = "chr";
        excludeShellChecks = [ "SC1091" ];
        runtimeInputs = [
          python
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
          export PYTHONPATH="$PWD/src''${PYTHONPATH:+:$PYTHONPATH}"
          source ${./nix/host-gpu.sh}
        '';
      };
      formatter.${system} = pkgs.nixfmt;
    };
}
