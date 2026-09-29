{
  description = "oxmux dev shell";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      # winit/softbuffer dlopen these at runtime for the desktop simulator.
      simLibs = with pkgs; [ wayland libxkbcommon libx11 libxcursor libxi libxrandr ];
    in {
      devShells.${system}.default = pkgs.mkShell {
        # rustup for the toolchain; the rest is for scripts/build-image.sh.
        packages = with pkgs; [ rustup util-linux e2fsprogs jq xz gptfdisk ];
        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath simLibs;
      };
    };
}
