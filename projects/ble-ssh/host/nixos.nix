# Import this module in the host's NixOS configuration for dual-mode watches.
# Applying it is an explicit system configuration operation; the client itself
# never changes Bluetooth settings.
{ pkgs, ... }:
{
  hardware.bluetooth.enable = true;
  hardware.bluetooth.package = pkgs.callPackage ./bluez-package.nix { };
  hardware.bluetooth.settings.General.Experimental = true;
}
