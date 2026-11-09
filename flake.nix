{
  description = "GPU-resident Dawn C++ compositor experiment";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/6cdc7fc76e8bf7fde9fa43a849fcaaa70e230dee";
  outputs =
    { nixpkgs, ... }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
      dawn = pkgs.stdenvNoCC.mkDerivation {
        pname = "dawn-webgpu";
        version = "20261106.204713";
        src = pkgs.fetchurl {
          url = "https://github.com/google/dawn/releases/download/v20261106.204713/Dawn-66f899221d41dbc6d6257350c4b654a9b375c7f2-ubuntu-latest-Release.tar.gz";
          hash = "sha256-hGkCLqiRBcM2ObE0LIHJrfWTrAskNvKDSO3RaeiR5fY=";
        };
        dontConfigure = true;
        dontBuild = true;
        dontFixup = true;
        installPhase = "mkdir -p $out; cp -r include lib64 $out/; ln -s lib64 $out/lib";
      };
      libraries = pkgs.lib.makeLibraryPath [
        pkgs.vulkan-loader
        pkgs.libX11
        pkgs.libXext
        pkgs.libglvnd
      ];
      preparationPython = pkgs.python3.withPackages (ps: [
        ps.av
        ps.numpy
        ps.opencv4
        ps.pillow
        ps.pyarrow
      ]);
      deps = [
        dawn
        pkgs.ffmpeg
        pkgs.vulkan-headers
        pkgs.vulkan-loader
        pkgs.nlohmann_json
      ];
      native = pkgs.clangStdenv.mkDerivation {
        pname = "chr-dawn";
        version = "0.1.0";
        src = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            ./CMakeLists.txt
            ./native
          ];
        };
        nativeBuildInputs = [
          pkgs.cmake
          pkgs.ninja
          pkgs.pkg-config
        ];
        buildInputs = deps;
        doCheck = true;
      };
      app = pkgs.writeShellApplication {
        name = "chr-dawn";
        excludeShellChecks = [ "SC1091" ];
        text = ''
          export LD_LIBRARY_PATH="${libraries}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
          source ${./nix/host-gpu.sh}
          exec ${native}/bin/chr-dawn "$@"
        '';
      };
    in
    {
      packages.${system} = {
        default = app;
        inherit dawn native;
      };
      apps.${system}.default = {
        type = "app";
        program = "${app}/bin/chr-dawn";
      };
      devShells.${system} = {
        default = pkgs.mkShell.override { stdenv = pkgs.clangStdenv; } {
          packages = [
            pkgs.cmake
            pkgs.ninja
            pkgs.pkg-config
            pkgs.nixfmt
            pkgs.python3
            pkgs.vulkan-validation-layers
            pkgs.iproute2
          ];
          buildInputs = deps;
          LD_LIBRARY_PATH = libraries;
          shellHook = ''
            source ${./nix/host-gpu.sh}
          '';
        };
        prepare = pkgs.mkShell {
          packages = [
            preparationPython
            pkgs.ffmpeg
          ];
        };
      };
      formatter.${system} = pkgs.nixfmt;
    };
}
