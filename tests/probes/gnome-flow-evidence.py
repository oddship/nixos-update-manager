#!/usr/bin/env python3
"""Record and verify the synthetic full update flow inside the GNOME guest."""
import argparse
import hashlib
import json
from pathlib import Path
import socket
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('phase', choices=['baseline', 'prepared', 'complete'])
parser.add_argument('--base', type=Path, default=Path.home() / 'updates-flow')
parser.add_argument('--output', type=Path, default=Path('/tmp/xchg/gnome-flow-evidence.json'))
parser.add_argument('--expected-value', default='new')
parser.add_argument('--host', default='demo')
args = parser.parse_args()
assert socket.gethostname() == 'updates-demo', 'This probe runs only in the disposable guest.'
repo = args.base / 'config'

def git(*arguments):
    return subprocess.check_output(['git', '-C', str(repo), *arguments], timeout=15).decode().strip()

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def unrelated_index():
    return [line for line in git('ls-files', '--stage').splitlines() if not line.endswith('\tflake.lock')]

def system():
    return {'running': str(Path('/run/current-system').resolve()),
            'profile': str(Path('/nix/var/nix/profiles/system').resolve())}

if args.phase == 'baseline':
    evidence = {'schema_version': 1, 'fixture': 'real NixOS GNOME full update flow',
                'seeded_candidate': False, 'authorization': 'native graphical polkit',
                'expected_value': args.expected_value,
                'baseline': {'head': git('rev-parse', 'HEAD'), 'system': system(),
                             'lock_sha256': digest(repo / 'flake.lock'),
                             'local_sha256': digest(repo / 'local.nix'),
                             'unrelated_index': unrelated_index(),
                             'index_sha256': digest(repo / '.git/index')}}
else:
    evidence = json.loads(args.output.read_text())
    candidates = [json.loads(path.read_text()) for path in
                  (Path.home() / '.local/state/nixos-updates').glob('candidate-*/state.json')]
    candidates = [record for record in candidates
                  if record['repository']['root'] == str(repo) and record['host'] == args.host]
    candidate = max(candidates, key=lambda record: record.get('created_at_millis', record['created_at'] * 1000))
    baseline = evidence['baseline']
    assert digest(repo / 'local.nix') == baseline['local_sha256'], 'Working edits changed.'
    assert unrelated_index() == baseline['unrelated_index'], 'Unrelated staged entries changed.'
    if args.phase == 'prepared':
        assert candidate['state'] == 'ready', candidate
        assert git('rev-parse', 'HEAD') == baseline['head']
        assert digest(repo / 'flake.lock') == baseline['lock_sha256']
        assert digest(repo / '.git/index') == baseline['index_sha256']
        assert system() == baseline['system']
        prepared = Path(candidate['prepared_system'])
        assert (prepared / 'etc/updates-fixture').read_text() == evidence.get('expected_value', 'new')
        evidence['prepared'] = {'candidate': candidate['id'], 'system': str(prepared),
                                'checkout_index_system_preserved': True}
    else:
        assert candidate['state'] == 'committed', candidate
        application = candidate['application']
        assert application['activation_completed']
        assert system() == {'running': candidate['prepared_system'], 'profile': candidate['prepared_system']}
        assert Path('/etc/updates-fixture').read_text() == evidence.get('expected_value', 'new')
        commit = application['commit']
        assert git('rev-parse', 'HEAD') == commit
        assert git('rev-parse', 'HEAD^') == baseline['head']
        assert git('diff-tree', '--no-commit-id', '--name-only', '-r', 'HEAD') == 'flake.lock'
        assert git('show', 'HEAD:flake.lock') == (repo / 'flake.lock').read_text().strip()
        evidence['complete'] = {'candidate': candidate['id'], 'system': system(), 'commit': commit,
                                'exact_system_verified': True, 'only_lock_committed': True,
                                'unrelated_staging_and_working_edits_preserved': True}
args.output.write_text(json.dumps(evidence, indent=2) + '\n')
print(json.dumps(evidence, indent=2))
