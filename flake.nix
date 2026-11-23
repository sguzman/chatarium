{
  description = "Chatarium native workstation (Linux, per-crate Rust builds)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/619c39f6eac5d1ed6b8f073a9414baf7034b2356";
    rust-overlay = {
      url = "github:oxalica/rust-overlay/ea2084066e0f3fed86a76a2654658ed2eb998046";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crate2nix = {
      url = "github:nix-community/crate2nix/1cb60331b14f15dad145dbd4e207c1ec110e5675";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, rust-overlay, crate2nix }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      };
      toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      native = with pkgs; [
        libGL
        libxkbcommon
        wayland
        xorg.libX11
        xorg.libXcursor
        xorg.libXi
        xorg.libXrandr
        xorg.libxcb
        fontconfig
        freetype
      ];
      runtime = with pkgs; [
        nodejs_22
        git
        pnpm
        libsecret
        fish
        coreutils
      ];
      crateTools = pkgs.callPackage (crate2nix + "/tools.nix") { };
      # Generate against Cargo.lock using the workspace's pinned Cargo.
      # Import-from-derivation is intentional; dependencies build per crate.
      generated = crateTools.generatedCargoNix {
        name = "chatarium";
        src = self.outPath;
        cargo = toolchain;
      };
      cargoNix = pkgs.callPackage generated {
        buildRustCrateForPkgs = p: p.buildRustCrate.override {
          rustc = toolchain;
          cargo = toolchain;
        };
      };
    in {
      # This is a compilation target, NOT the finished runnable distribution.
      # The authenticated sidecar and runtime path are separate acceptance work.
      packages.${system}.desktop-unwrapped =
        cargoNix.workspaceMembers.chatarium-desktop.build;

      devShells.${system}.default = pkgs.mkShell {
        packages = [ toolchain pkgs.pkg-config pkgs.crate2nix ] ++ runtime;
        buildInputs = native;
        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath native;
      };
    };
}
