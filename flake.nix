{
  description = "oxmux: Rust userland for Anbernic H700 handhelds";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, rust-overlay, ... }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; overlays = [ rust-overlay.overlays.default ]; };
      # Target-architecture packages come straight from cache.nixos.org's aarch64 builds.
      target = nixpkgs.legacyPackages.aarch64-linux;

      oxmux = pkgs.callPackage ./nix/oxmux.nix { src = ./.; };
      image-builder = pkgs.callPackage ./nix/image-builder.nix {
        inherit oxmux;
        src = ./.;
        busybox = target.pkgsStatic.busybox;
        tools = with pkgs; [ util-linux e2fsprogs jq mtools dosfstools
          squashfsTools binutils cpio gzip curl coreutils findutils fakeroot ];
      };

      # QEMU "virt" test machine: stock kernel, oxmux as PID 1 from an initramfs.
      vm = pkgs.callPackage ./nix/vm.nix {
        inherit oxmux;
        kernel = target.linux_6_12;
        busybox = target.pkgsStatic.busybox;
        profile = ./config/qemu;
      };

      # Cross toolchain for the bootloader (small C builds; the rest comes from the cache).
      cross = pkgs.pkgsCross.aarch64-multiplatform;
      uboot = cross.callPackage ./nix/uboot.nix { };

      # winit/softbuffer dlopen these at runtime for the desktop simulator.
      simLibs = with pkgs; [ wayland libxkbcommon libx11 libxcursor libxi libxrandr ];
    in
    {
      packages.${system} = {
        inherit oxmux uboot image-builder;
        vm-initrd = vm.initrd;
        vm = vm.runner;
        default = vm.runner;
      };

      apps.${system} = {
        vm = { type = "app"; program = "${vm.runner}/bin/oxmux-vm"; };
        image = { type = "app"; program = "${image-builder}/bin/oxmux-image-builder"; };
      };

      devShells.${system}.default = pkgs.mkShell {
        # rustup for the toolchain; the rest is for scripts/build-image.sh and the VM.
        packages = with pkgs; [ rustup util-linux e2fsprogs jq xz gptfdisk qemu
          mtools dosfstools squashfsTools binutils cpio gzip curl fakeroot ];
        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath simLibs;
      };
    };
}
