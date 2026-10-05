{ pkgs, package, module }: pkgs.testers.runNixOSTest {
  name = "nixos-update-manager-authenticated-helper";
  qemu.package = pkgs.qemu_kvm;
  nodes.machine = { ... }: {
    imports = [ module ];
    # Exercise the legacy option alias; vm-apply uses the canonical option.
    services.nixos-updates.enable = true;
    virtualisation.memorySize = 1024;
    virtualisation.useBootLoader = true;
    virtualisation.diskSize = 4096;
    nix.settings.experimental-features = [ "nix-command" "flakes" ];
    users.users.tester = { isNormalUser = true; extraGroups = [ "wheel" ]; password = "disposable-vm-only"; };
    environment.etc."updates-probe".text = "baseline";
    specialisation.candidate.configuration.environment.etc."updates-probe".text = pkgs.lib.mkForce "candidate";
    system.stateVersion = "26.05";
  };
  testScript = ''
    import json, shlex
    machine.start()
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("polkit.service")
    baseline = machine.succeed("readlink -f /run/current-system").strip()
    candidate = machine.succeed("readlink -f /run/current-system/specialisation/candidate").strip()
    executable = "${package}/bin/nixos-update-manager-helper"
    machine.succeed("test $(readlink -f ${package}/bin/nixos-updates-helper) = $(readlink -f " + executable + ")")
    pkexec = "/run/wrappers/bin/pkexec --disable-internal-agent "
    args = executable + " --system " + candidate + " --expected-running " + baseline
    # An unprivileged direct call is rejected.
    machine.fail("su - tester -c " + shlex.quote(args))
    # Package/module policy alone does not authorize passwordless activation.
    machine.fail("su - tester -c " + shlex.quote(pkexec + args))
    assert machine.succeed("readlink -f /run/current-system").strip() == baseline
    assert machine.succeed("readlink -f /nix/var/nix/profiles/system").strip() == baseline
    # Test-only authorization exercises pkexec/root separation. It is never
    # shipped by the module and does not establish graphical auth coverage.
    rule = 'polkit.addRule(function(action, subject) { if (action.id == "io.github.oddship.NixOSUpdates.apply" && subject.user == "tester") return polkit.Result.YES; });'
    machine.succeed("mkdir -p /etc/polkit-1/rules.d; printf %s " + shlex.quote(rule) + " > /etc/polkit-1/rules.d/00-test-helper.rules")
    machine.succeed("systemctl restart polkit.service")
    # EOF after authentication is a refusal, not an activation.
    machine.fail("su - tester -c " + shlex.quote(pkexec + args + " < /dev/null"))
    assert machine.succeed("readlink -f /run/current-system").strip() == baseline
    output = machine.succeed("su - tester -c " + shlex.quote("printf 'activate\\n' | " + pkexec + args))
    messages = [json.loads(line) for line in output.splitlines() if line.startswith('{')]
    assert messages[0]["phase"] == "authenticated"
    assert messages[-1]["phase"] == "applied"
    assert messages[-1]["status"]["running"] == candidate
    assert messages[-1]["status"]["profile"] == candidate
    assert machine.succeed("cat /etc/updates-probe").strip() == "candidate"
    # The old baseline cannot authorize another activation after generation change.
    machine.fail(executable + " --system " + baseline + " --expected-running " + baseline + " < /dev/null")
    machine.succeed("printf 'activate\\n' | " + executable + " --system " + baseline + " --expected-running " + candidate)
    assert machine.succeed("readlink -f /run/current-system").strip() == baseline
    assert machine.succeed("readlink -f /nix/var/nix/profiles/system").strip() == baseline
  '';
}
