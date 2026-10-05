#!/usr/bin/env python3
"""Check and prepare a real synthetic NixOS flake; never activate it."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, default=Path(__file__).resolve().parents[2] / 'target/debug/nixos-update-manager')
parser.add_argument('--output', type=Path)
args = parser.parse_args()
binary = args.binary.resolve(strict=True)
lock = json.loads((Path(__file__).resolve().parents[2] / "flake.lock").read_text())
nixpkgs_revision = lock["nodes"]["nixpkgs"]["locked"]["rev"]

def run(command):
    return subprocess.check_output([str(arg) for arg in command], text=True)

def status():
    return (os.path.realpath('/run/current-system'), os.path.realpath('/nix/var/nix/profiles/system'))

with tempfile.TemporaryDirectory(prefix='nixos-updates-real-prepare-') as tmp:
    root = Path(tmp)
    source, dependency, state = root / 'config', root / 'input', root / 'state'
    for repo in (source, dependency):
        repo.mkdir()
        run(['git', '-C', repo, 'init', '-q'])
        run(['git', '-C', repo, 'config', 'user.name', 'NixOS Fixture'])
        run(['git', '-C', repo, 'config', 'user.email', 'fixture@example.invalid'])
        run(['git', '-C', repo, 'config', 'commit.gpgSign', 'false'])
        run(['git', '-C', repo, 'config', 'core.hooksPath', '.git/hooks'])
    def revision(value):
        (dependency / 'flake.nix').write_text('{ outputs = { self }: { value = "' + value + '"; }; }')
        run(['git', '-C', dependency, 'add', '.'])
        run(['git', '-C', dependency, 'commit', '-qm', value])
    revision('old')
    (source / 'flake.nix').write_text('''{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/''' + nixpkgs_revision + '''";
  inputs.fixture.url = "git+file://''' + str(dependency) + '''";
  outputs = { nixpkgs, fixture, ... }: {
    nixosConfigurations.fixture = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [ ({ ... }: {
        networking.hostName = "updates-fixture";
        system.stateVersion = "26.05";
        fileSystems."/" = { device = "/dev/disk/by-label/nixos"; fsType = "ext4"; };
        boot.loader.grub.devices = [ "nodev" ];
        environment.etc."updates-fixture".text = fixture.value;
      }) ];
    };
  };
}''')
    run(['git', '-C', source, 'add', '.'])
    run(['nix', '--extra-experimental-features', 'nix-command flakes', '--max-jobs', '1', '--cores', '2', 'flake', 'lock', source])
    run(['git', '-C', source, 'add', '.'])
    run(['git', '-C', source, 'commit', '-qm', 'baseline'])
    revision('new')
    command = [binary, '--state-dir', state]
    before = json.loads(run([binary, 'inspect', source]))['fingerprint']
    system_before = status()
    started = time.monotonic()
    checked = json.loads(run([*command, 'check', source, '--host', 'fixture', '--update-input', 'fixture', '--exclude-input', 'nixpkgs']))
    check_seconds = time.monotonic() - started
    assert checked['state'] == 'updates_found', checked
    assert checked['changed_inputs'] == ['fixture'], checked['changed_inputs']
    assert json.loads(run([binary, 'inspect', source]))['fingerprint'] == before
    assert status() == system_before
    started = time.monotonic()
    prepared = json.loads(run([*command, 'prepare', checked['id']]))
    prepare_seconds = time.monotonic() - started
    assert prepared['state'] == 'ready', prepared
    system = Path(prepared['prepared_system'])
    assert (system / 'bin/switch-to-configuration').is_file()
    assert (system / 'nixos-version').is_file()
    assert (system / 'etc/updates-fixture').read_text() == 'new'
    assert json.loads(run([binary, 'inspect', source]))['fingerprint'] == before
    assert status() == system_before
    run([*command, 'fresh', checked['id']])
    evidence = {'real_nixos_check_prepare': True, 'seeded_candidate': False,
                'activated': False, 'checkout_and_index_preserved': True,
                'running_system_and_profile_preserved': True,
                'updated_inputs': checked['changed_inputs'],
                'check_seconds': round(check_seconds, 2), 'prepare_seconds': round(prepare_seconds, 2),
                'prepared_system': str(system), 'nixpkgs_revision': nixpkgs_revision}
    output = json.dumps(evidence, indent=2) + '\n'
    if args.output:
        args.output.write_text(output)
    print(output, end='')
