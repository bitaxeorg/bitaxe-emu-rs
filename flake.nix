{
  description = "bitaxe-emu-rs: Bitaxe emulator, the ESP32-S3 on rax with the board around it";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # The Rust toolchain of rust-toolchain.toml (a dated nightly).
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      # The window needs Wayland.
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      # `pkgs.bitaxe-emu`, built with the nixpkgs the overlay is applied to.
      overlays.default = final: _prev: {
        bitaxe-emu = final.callPackage ./nix/package.nix {
          rust-bin = rust-overlay.lib.mkRustBin { } final;
          rax = final.callPackage ./nix/rax.nix { };
        };
      };

      nixosModules.default = import ./nix/module.nix self;

      packages = forAllSystems (pkgs: {
        default = (self.overlays.default pkgs pkgs).bitaxe-emu;
      });

      devShells = forAllSystems (
        pkgs:
        let
          rust = (rust-overlay.lib.mkRustBin { } pkgs).fromRustupToolchainFile ./rust-toolchain.toml;
          # Libraries the BLIT desktop backend opens at run time.
          runtimeLibs = with pkgs; [
            wayland
            libxkbcommon
            fontconfig
            vulkan-loader
          ];
          fonts = "${pkgs.dejavu_fonts}/share/fonts/truetype";
        in
        {
          default = pkgs.mkShell {
            packages = [
              rust
              pkgs.pkg-config
              # flash images of the firmware (`espflash save-image --merge`)
              pkgs.espflash
            ]
            ++ runtimeLibs;

            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
            # Fonts of the window, embedded at build time.
            BITAXE_EMU_FONT = "${fonts}/DejaVuSans.ttf";
            BITAXE_EMU_FONT_MONO = "${fonts}/DejaVuSansMono.ttf";
          };
        }
      );

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
