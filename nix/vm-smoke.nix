{ pkgs }: pkgs.testers.runNixOSTest {
  name = "nixos-update-manager-vm-smoke";
  # Reuse the same software-emulation-capable QEMU as the graphical guest.
  qemu.package = pkgs.qemu_kvm;
  nodes.machine = { ... }: {
    virtualisation.memorySize = 1024;
    virtualisation.useBootLoader = true;
    virtualisation.diskSize = 4096;
    nix.settings.experimental-features = [ "nix-command" "flakes" ];
    environment.systemPackages = [ pkgs.git ];
    environment.etc."updates-probe".text = "baseline";
    specialisation.candidate.configuration = {
      environment.etc."updates-probe".text = pkgs.lib.mkForce "candidate";
    };
    system.stateVersion = "26.05";
  };
  testScript = ''
    machine.start()
    machine.wait_for_unit("multi-user.target")
    machine.succeed("test -e /run/current-system/bin/switch-to-configuration")
    machine.succeed("git --version; nix --version")
    baseline = machine.succeed("readlink -f /run/current-system").strip()
    candidate = machine.succeed("readlink -f /run/current-system/specialisation/candidate").strip()
    assert baseline != candidate
    machine.succeed("nix-env -p /nix/var/nix/profiles/system --set " + candidate)
    machine.succeed(candidate + "/bin/switch-to-configuration switch")
    assert machine.succeed("readlink -f /run/current-system").strip() == candidate
    assert machine.succeed("readlink -f /nix/var/nix/profiles/system").strip() == candidate
    assert machine.succeed("cat /etc/updates-probe").strip() == "candidate"
    # Explicitly restore the disposable guest and verify the same boundary again.
    machine.succeed("nix-env -p /nix/var/nix/profiles/system --set " + baseline)
    machine.succeed(baseline + "/bin/switch-to-configuration switch")
    assert machine.succeed("readlink -f /run/current-system").strip() == baseline
    assert machine.succeed("cat /etc/updates-probe").strip() == "baseline"
  '';
}
