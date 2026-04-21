{
  description = "Nix build for tyler";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs =
    { self, nixpkgs, ... }:
    let
      supportedSystems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];

      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          nativeBuildInputs = [
            pkgs.cmake
            pkgs.llvmPackages.clang
            pkgs.makeWrapper
            pkgs.pkg-config
          ];
          buildInputs = [
            pkgs.libtiff
            pkgs.proj
            pkgs.sqlite
          ];
        in
        rec {
          default = tyler;

          tyler = pkgs.rustPlatform.buildRustPackage {
            pname = "tyler";
            version = "0.4.1-alpha1";

            src = self;
            cargoLock.lockFile = ./Cargo.lock;

            inherit nativeBuildInputs buildInputs;

            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            PROJ_DATA = "${pkgs.proj}/share/proj";

            doCheck = false;

            postInstall = ''
              wrapProgram "$out/bin/tyler" \
                --set-default PROJ_DATA "${pkgs.proj}/share/proj"
            '';
          };
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        {
          default = pkgs.mkShell {
            packages = [
              pkgs.cargo
              pkgs.clippy
              pkgs.cmake
              pkgs.llvmPackages.clang
              pkgs.pkg-config
              pkgs.rustc
              pkgs.rustfmt
            ];

            buildInputs = [
              pkgs.libtiff
              pkgs.proj
              pkgs.sqlite
            ];

            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            PROJ_DATA = "${pkgs.proj}/share/proj";
          };
        }
      );
    };
}
