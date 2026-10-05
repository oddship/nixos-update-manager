{ package, indicator ? null }:
{ config, lib, pkgs, options, ... }:
let
  cfg = config.services.nixos-update-manager;
  policy = pkgs.writeTextDir "share/polkit-1/actions/io.github.oddship.NixOSUpdates.policy" ''
    <?xml version="1.0" encoding="UTF-8"?>
    <!DOCTYPE policyconfig PUBLIC "-//freedesktop//DTD PolicyKit Policy Configuration 1.0//EN" "http://www.freedesktop.org/standards/PolicyKit/1/policyconfig.dtd">
    <policyconfig>
      <action id="io.github.oddship.NixOSUpdates.apply">
        <description>Apply the prepared NixOS system</description>
        <message>Authentication is required to activate the reviewed NixOS system.</message>
        <defaults>
          <allow_any>no</allow_any>
          <allow_inactive>auth_admin</allow_inactive>
          <allow_active>auth_admin</allow_active>
        </defaults>
        <annotate key="org.freedesktop.policykit.exec.path">${cfg.package}/bin/nixos-update-manager-helper</annotate>
      </action>
    </policyconfig>
  '';
in {
  # Keep old configurations working while new installations use the public name.
  imports = map (suffix: lib.mkRenamedOptionModule
    ([ "services" "nixos-updates" ] ++ suffix)
    ([ "services" "nixos-update-manager" ] ++ suffix))
    [ [ "enable" ] [ "package" ] [ "indicator" "enable" ] ];
  options.services.nixos-update-manager = {
    enable = lib.mkEnableOption "the NixOS Update Manager application and authenticated exact-system helper";
    indicator.enable = lib.mkEnableOption "installing the optional GNOME top-bar extension (enable it in GNOME Extensions)";
    package = lib.mkOption { type = lib.types.package; default = package; description = "Native updater package."; };
  };
  config = lib.mkIf cfg.enable {
    assertions = [ { assertion = !cfg.indicator.enable || indicator != null;
      message = "The top-bar indicator requires the flake's GNOME extension package."; } ];
    environment.systemPackages = [ cfg.package policy ] ++ lib.optional cfg.indicator.enable indicator;
    security.polkit = { enable = true; } // lib.optionalAttrs (options.security.polkit ? enablePkexecWrapper) {
      enablePkexecWrapper = true;
    };
  };
}
