{ pkgs, package, module }:
let
  fixturePath = pkgs.buildEnv {
    name = "failed-activation-fixture-path";
    ignoreCollisions = true;
    paths = [ pkgs.bash pkgs.coreutils pkgs.shadow pkgs.util-linux pkgs.git package ];
  };
  failedActivation = pkgs.runCommand "nixos-system-failed-activation-fixture" { } ''
    mkdir -p $out/bin
    echo fixture > $out/nixos-version
    # Keep the test driver's shell tools available after the injected link change.
    ln -s ${fixturePath} $out/sw
    cat > $out/bin/switch-to-configuration <<'SCRIPT'
    #!${pkgs.bash}/bin/bash
    set -eu
    system="$(${pkgs.coreutils}/bin/dirname "$(${pkgs.coreutils}/bin/dirname "$(${pkgs.coreutils}/bin/readlink -f "$0")")")"
    ${pkgs.coreutils}/bin/ln -sfn "$system" /run/current-system
    echo 'injected activation failure after system links changed' >&2
    exit 1
    SCRIPT
    chmod 755 $out/bin/switch-to-configuration
  '';
  seed = pkgs.writeScript "seed-exact-apply-fixture" ''
    #!${pkgs.python3}/bin/python3
    import json, os, pathlib, subprocess, sys, time
    repo = pathlib.Path('/home/tester/config')
    state = pathlib.Path('/home/tester/state')
    candidate_id, baseline, system, revision = sys.argv[1:]
    def git(*args):
        return subprocess.check_output(['${pkgs.git}/bin/git', '-C', str(repo), *args])
    if not repo.exists():
        repo.mkdir()
        git('init', '-q')
        git('config', 'user.name', 'Fixture')
        git('config', 'user.email', 'fixture@example.invalid')
        git('config', 'commit.gpgSign', 'false')
        git('config', 'core.hooksPath', '.git/hooks')
        (repo / 'flake.nix').write_text('{ outputs = { self }: { nixosConfigurations.fixture = {}; }; }')
        (repo / 'flake.lock').write_text('{}')
        (repo / 'module.nix').write_text('baseline')
        git('add', '.')
        git('commit', '-qm', 'fixture')
        (repo / 'module.nix').write_text('staged')
        git('add', 'module.nix')
        (repo / 'module.nix').write_text('unstaged over staged')
    state.mkdir(exist_ok=True, mode=0o700)
    directory = state / candidate_id
    directory.mkdir(mode=0o700)
    inspected = json.loads(subprocess.check_output(['${package}/bin/nixos-update-manager', 'inspect', str(repo)]))
    original = (repo / 'flake.lock').read_bytes()
    candidate = json.dumps({'fixture_revision':revision}).encode()
    import hashlib
    digest = lambda data: hashlib.sha256(data).hexdigest()
    (directory / 'original.lock').write_bytes(original)
    (directory / 'candidate.lock').write_bytes(candidate)
    (directory / 'gc-root').symlink_to(system)
    record = dict(schema_version=1, id=candidate_id, repository=inspected['repository'], fingerprint=inspected['fingerprint'],
        host='fixture', policy=dict(update=[],exclude=[]), created_at=int(time.time()), created_at_millis=int(time.time()*1000),
        state='ready', changed_inputs=['fixture'], skipped_inputs=[], current_drv=None, candidate_drv=None,
        candidate_lock_hash=digest(candidate), prepared_system=system, review_baseline=baseline, closure_diff='Seeded real-NixOS output for activation boundary testing.',
        error=None, operation_id=None, application=None)
    (directory / 'state.json').write_text(json.dumps(record))
  '';
in pkgs.testers.runNixOSTest {
  name = "nixos-update-manager-exact-apply-and-commit";
  qemu.package = pkgs.qemu_kvm;
  nodes.machine = { ... }: {
    imports = [ module ];
    services.nixos-update-manager.enable = true;
    virtualisation.memorySize = 1024;
    virtualisation.useBootLoader = true;
    virtualisation.diskSize = 4096;
    nix.settings.experimental-features = [ "nix-command" "flakes" ];
    users.users.tester = { isNormalUser = true; extraGroups = [ "wheel" ]; password = "disposable-vm-only"; };
    environment.systemPackages = [ pkgs.git ];
    # The test script itself is not part of the guest closure.
    system.extraDependencies = [ seed failedActivation ];
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
    machine.succeed("${package}/bin/nixos-updates --version | grep nixos-update-manager")
    command = "${package}/bin/nixos-update-manager --state-dir /home/tester/state "
    def user(cmd): return machine.succeed("su - tester -c " + shlex.quote(cmd))
    def seed_record(name, revision):
        user("${seed} " + name + " " + baseline + " " + candidate + " " + revision)
    def show(name): return json.loads(user(command + "show " + name))
    def git(args): return user("git -C /home/tester/config " + args).strip()
    seed_record("candidate-refusal", "one")
    initial_head = git("rev-parse HEAD")
    initial_index = machine.succeed("sha256sum /home/tester/config/.git/index").split()[0]
    # Without an authentication agent, refusal changes neither checkout nor system.
    machine.fail("su - tester -c " + shlex.quote(command + "apply candidate-refusal"))
    assert show("candidate-refusal")["state"] == "failed"
    assert machine.succeed("readlink -f /run/current-system").strip() == baseline
    assert machine.succeed("cat /home/tester/config/flake.lock").strip() == "{}"
    assert machine.succeed("sha256sum /home/tester/config/.git/index").split()[0] == initial_index
    # Test-only authorization; graphical authentication is a separate guest gate.
    rule = 'polkit.addRule(function(action, subject) { if (action.id == "io.github.oddship.NixOSUpdates.apply" && subject.user == "tester") return polkit.Result.YES; });'
    machine.succeed("printf %s " + shlex.quote(rule) + " > /etc/polkit-1/rules.d/00-test-updater.rules")
    machine.succeed("systemctl restart polkit.service")
    seed_record("candidate-success", "one")
    plan = json.loads(user(command + "apply-plan candidate-success"))
    assert plan["can_commit"] and plan["includes_local_edits"]
    applied = json.loads(user(command + "apply candidate-success --commit --message 'flake: fixture one'"))
    assert applied["state"] == "committed", applied
    assert applied["application"]["actual"]["running"] == candidate
    assert applied["application"]["actual"]["profile"] == candidate
    assert git("show HEAD:flake.lock") == '{"fixture_revision": "one"}'
    assert git("show HEAD:module.nix") == "baseline"
    assert git("show :module.nix") == "staged"
    assert machine.succeed("cat /home/tester/config/module.nix").strip() == "unstaged over staged"
    assert git("rev-parse HEAD") != initial_head
    assert machine.succeed("cat /etc/updates-probe").strip() == "candidate"
    # Restore only the disposable guest so the second application has a baseline.
    machine.succeed("printf 'activate\\n' | ${package}/bin/nixos-update-manager-helper --system " + baseline + " --expected-running " + candidate)
    seed_record("candidate-hook", "two")
    machine.succeed("printf '#!/bin/sh\\nexit 1\\n' > /home/tester/config/.git/hooks/pre-commit; chmod 755 /home/tester/config/.git/hooks/pre-commit")
    head_before_hook = git("rev-parse HEAD")
    machine.fail("su - tester -c " + shlex.quote(command + "apply candidate-hook --commit --message 'flake: fixture two'"))
    failed_commit = show("candidate-hook")
    assert failed_commit["state"] == "commit_needs_attention", failed_commit
    assert failed_commit["application"]["actual"]["running"] == candidate
    assert git("rev-parse HEAD") == head_before_hook
    assert machine.succeed("readlink -f /run/current-system").strip() == candidate
    machine.succeed("rm /home/tester/config/.git/hooks/pre-commit")
    # Commit-only retry must not invoke authentication or reactivate services.
    retried = json.loads(user(command + "commit candidate-hook"))
    assert retried["state"] == "committed", retried
    assert git("show HEAD:flake.lock") == '{"fixture_revision": "two"}'
    assert git("show :module.nix") == "staged"
    assert machine.succeed("readlink -f /run/current-system").strip() == candidate
    machine.succeed("printf 'activate\\n' | ${package}/bin/nixos-update-manager-helper --system " + baseline + " --expected-running " + candidate)
    # Inject a failing activation that changes both links before returning failure.
    # Matching links must not be reported as a successful service activation.
    user("${seed} candidate-partial " + baseline + " ${failedActivation} three")
    head_before_partial = git("rev-parse HEAD")
    machine.fail("su - tester -c " + shlex.quote(command + "apply candidate-partial --commit --message 'must not commit'"))
    partial = show("candidate-partial")
    assert partial["state"] == "apply_needs_attention", partial
    assert not partial["application"]["activation_completed"]
    assert partial["application"]["actual"]["running"] == "${failedActivation}"
    assert partial["application"]["actual"]["profile"] == "${failedActivation}"
    assert git("rev-parse HEAD") == head_before_partial
    assert machine.succeed("cat /home/tester/config/flake.lock").strip() == '{"fixture_revision": "three"}'
    # The injected program only replaced links, not service configuration.
    # Reset those test-only links; an incomplete fake system has no compatible init.
    machine.succeed("ln -sfn " + baseline + " /run/current-system")
    machine.succeed("${pkgs.nix}/bin/nix-env --profile /nix/var/nix/profiles/system --set " + baseline)
  '';
}
