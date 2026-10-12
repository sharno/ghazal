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
      # x86_64 workstations/CI, and aarch64 Linux (e.g. a container on Apple Silicon),
      # where the device packages build natively.
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forSystem = system:
        let
          pkgs = import nixpkgs { inherit system; overlays = [ rust-overlay.overlays.default ]; };
          # Target-architecture packages come straight from cache.nixos.org's aarch64 builds.
          target = nixpkgs.legacyPackages.aarch64-linux;

          oxmux = pkgs.callPackage ./nix/oxmux.nix { src = ./.; };
          # Cross toolchain for the bootloader and kernel; native on aarch64 hosts.
          cross = if system == "aarch64-linux" then pkgs else pkgs.pkgsCross.aarch64-multiplatform;
          uboot = cross.callPackage ./nix/uboot.nix { };
          h700-kernel = cross.callPackage ./nix/h700-kernel.nix { };
          h700-debug-kernel = cross.callPackage ./nix/h700-kernel.nix { debug = true; };
          image-builder = pkgs.callPackage ./nix/image-builder.nix {
            inherit oxmux uboot;
            kernel = h700-kernel;
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

          # winit/softbuffer dlopen these at runtime for the desktop simulator.
          simLibs = with pkgs; [ wayland libxkbcommon libx11 libxcursor libxi libxrandr ];
        in
        {
          packages = {
            inherit oxmux uboot image-builder h700-kernel h700-debug-kernel;
            vm-initrd = vm.initrd;
            vm = vm.runner;
            default = vm.runner;
          };

          apps = {
            vm = { type = "app"; program = "${vm.runner}/bin/oxmux-vm"; };
            image = { type = "app"; program = "${image-builder}/bin/oxmux-image-builder"; };
          };

          devShells.default = pkgs.mkShell {
            # rustup for the toolchain; the rest is for scripts/build-image.sh and the VM.
            packages = with pkgs; [ rustup util-linux e2fsprogs jq xz gptfdisk qemu
              mtools dosfstools squashfsTools binutils cpio gzip curl fakeroot ];
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath simLibs;
          };
        };
      perSystem = nixpkgs.lib.genAttrs systems forSystem;
    in
    {
      packages = nixpkgs.lib.mapAttrs (_: o: o.packages) perSystem;
      apps = nixpkgs.lib.mapAttrs (_: o: o.apps) perSystem;
      devShells = nixpkgs.lib.mapAttrs (_: o: o.devShells) perSystem;
    };
}
