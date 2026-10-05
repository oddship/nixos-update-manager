{ pkgs, modulesPath, ... }: {
  imports = [ (modulesPath + "/virtualisation/qemu-vm.nix") ];
  boot.kernelParams = [ "console=ttyS0,115200" "console=tty0" ];
  networking.hostName = "updates-demo";
  system.stateVersion = "26.05";
  nix.settings.experimental-features = [ "nix-command" "flakes" ];
  services.xserver.enable = true;
  services.displayManager.gdm.enable = true;
  services.desktopManager.gnome.enable = true;
  services.displayManager.autoLogin = { enable = true; user = "tester"; };
  users.users.tester = {
    isNormalUser = true;
    extraGroups = [ "wheel" ];
    initialPassword = "disposable-vm-only";
  };
  environment.systemPackages = with pkgs; [ git python3 dconf-editor ];
  # Let disposable fixtures use the same pinned Nixpkgs without a hard-coded store path.
  environment.etc."nixos-update-manager-test/nixpkgs-source".source = pkgs.path;
  virtualisation = {
    memorySize = 4096;
    cores = 2;
    diskSize = 16384;
    graphics = true;
    useBootLoader = true;
    # No host home, configuration checkout, credentials, or writable share.
    sharedDirectories = {};
    # qemu-vm already selects kvm:tcg, which falls back without /dev/kvm.
    qemu.options = [ "-vga virtio" ];
  };
}
