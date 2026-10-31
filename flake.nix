{
  description = "ghazal: Rust userland for Anbernic H700 handhelds";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # ROCKNIX's source: kernel and U-Boot patches, panel descriptions, PPSSPP
    # defaults and the emulator core recipes. No ROCKNIX binaries are used.
    rocknix = {
      url = "github:ROCKNIX/distribution/7cffc96617df2a051af091182e8a3d270fd794a4";
      flake = false;
    };
  };

  outputs = { nixpkgs, rust-overlay, rocknix, ... }:
    let
      # x86_64 workstations/CI, and aarch64 Linux (e.g. a container on Apple Silicon),
      # where the device packages build natively.
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forSystem = system:
        let
          pkgs = import nixpkgs { inherit system; overlays = [ rust-overlay.overlays.default ]; };
          # Target-architecture packages, native aarch64 builds. Stock packages
          # come from cache.nixos.org; the runtime's slimmed graphics stack and
          # emulators are built.
          target = import nixpkgs { system = "aarch64-linux"; };
          runtime = import ./nix/runtime { pkgs = target; inherit rocknix; profile = ./config/rg40xxv-rocknix; };

          ghazal = pkgs.callPackage ./nix/ghazal.nix { src = ./.; };
          # Cross toolchain for the bootloader and kernel; native on aarch64 hosts.
          cross = if system == "aarch64-linux" then pkgs else pkgs.pkgsCross.aarch64-multiplatform;
          uboot = cross.callPackage ./nix/uboot.nix { inherit rocknix; };
          uboot-ddr3 = cross.callPackage ./nix/uboot.nix { inherit rocknix; memory = "DDR3"; };
          # Bluetooth is in the kernel source (nix/h700-kernel.nix, docs/bluetooth.md) but off in
          # every image until a kernel with it has booted on a handheld: set this to true to test it.
          bluetoothKernel = false;
          h700-kernel = cross.callPackage ./nix/h700-kernel.nix { inherit rocknix; bluetooth = bluetoothKernel; };
          h700-debug-kernel = cross.callPackage ./nix/h700-kernel.nix { inherit rocknix; debug = true; bluetooth = bluetoothKernel; };
          image-builder = pkgs.callPackage ./nix/image-builder.nix {
            inherit ghazal uboot uboot-ddr3 runtime;
            kernel = h700-kernel;
            src = ./.;
            busybox = target.pkgsStatic.busybox;
            sources = { nixpkgs = nixpkgs.rev; rocknix = rocknix.rev; };
            tools = with pkgs; [ util-linux e2fsprogs mtools dosfstools gzip coreutils fakeroot rauc ];
          };

          # QEMU "virt" test machine: stock kernel, ghazal as PID 1 from an initramfs.
          vm = pkgs.callPackage ./nix/vm.nix {
            inherit ghazal;
            kernel = target.linux_6_12;
            busybox = target.pkgsStatic.busybox;
            profile = ./config/qemu;
          };

          # Release gates (tools/release-check, docs/release-checks.md).
          release-check = pkgs.callPackage ./nix/release-check.nix { src = ./.; };
          vm-smoke = pkgs.callPackage ./nix/vm-smoke.nix { vm = vm.runner; };

          # winit/softbuffer dlopen these at runtime for the desktop simulator.
          simLibs = with pkgs; [ wayland libxkbcommon libx11 libxcursor libxi libxrandr ];
        in
        {
          packages = {
            inherit ghazal uboot uboot-ddr3 image-builder h700-kernel h700-debug-kernel;
            runtime = runtime.tree;
            inherit (runtime) mesa retroarch ppsspp moonlight decode-bench bluez;
            vm-initrd = vm.initrd;
            vm = vm.runner;
            inherit release-check vm-smoke;
            default = vm.runner;
          };

          # `nix build .#checks.<system>.runtime-layout` fails when the built
          # runtime breaks the shipped profile (cores, commands, gamepad profile).
          checks.runtime-layout = pkgs.runCommand "ghazal-runtime-layout" { } ''
            ${release-check}/bin/ghazal-release-check layout ${runtime.tree} ${./config/rg40xxv-rocknix}
            touch $out
          '';

          apps = {
            vm = { type = "app"; program = "${vm.runner}/bin/ghazal-vm"; };
            vm-smoke = { type = "app"; program = "${vm-smoke}/bin/ghazal-vm-smoke"; };
            image = { type = "app"; program = "${image-builder}/bin/ghazal-image-builder"; };
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
      checks = nixpkgs.lib.mapAttrs (_: o: o.checks) perSystem;
      devShells = nixpkgs.lib.mapAttrs (_: o: o.devShells) perSystem;
    };
}
