# NixOS module: `programs.bitaxe-emu.enable = true;` installs the emulator.
self:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.bitaxe-emu;
in
{
  options.programs.bitaxe-emu = {
    enable = lib.mkEnableOption "bitaxe-emu, the Bitaxe emulator";
    package = lib.mkOption {
      type = lib.types.package;
      default = (self.overlays.default pkgs pkgs).bitaxe-emu;
      defaultText = lib.literalExpression "bitaxe-emu built with the system's nixpkgs";
      description = "The bitaxe-emu package to install.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
  };
}
