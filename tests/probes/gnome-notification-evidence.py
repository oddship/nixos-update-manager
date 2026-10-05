#!/usr/bin/env python3
"""Verify closed-window preparation using a deliberately delayed guest build."""
import argparse
import json
from pathlib import Path
import socket
import subprocess
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('phase', choices=['closed', 'completed', 'reopened'])
parser.add_argument('--output', type=Path, default=Path('/tmp/xchg/gnome-notification-evidence.json'))
args = parser.parse_args()
assert socket.gethostname() == 'updates-demo'
state = Path.home() / '.local/state/nixos-updates'

def call(*arguments):
    return subprocess.check_output(arguments, timeout=10).decode().strip()

def app_owned():
    return call('busctl', '--user', 'call', 'org.freedesktop.DBus',
                '/org/freedesktop/DBus', 'org.freedesktop.DBus', 'NameHasOwner',
                's', 'io.github.oddship.NixOSUpdates') == 'b true'

if args.phase == 'closed':
    records = [json.loads(path.read_text()) for path in state.glob('candidate-*/state.json')]
    candidate = max(records, key=lambda record: record['created_at_millis'])
    assert candidate['state'] == 'preparing', candidate
    assert not app_owned()
    assert call('systemctl', '--user', 'show', 'nixos-updates-prepare.service',
                '-p', 'ActiveState', '--value') == 'active'
    assert call('systemctl', '--user', 'show', 'nixos-updates-prepare.service',
                '-p', 'Nice', '--value') == '10'
    claim = state / 'notifications' / (candidate['id'] + '-Ready')
    assert not claim.exists()
    evidence = {'schema_version': 1, 'candidate': candidate['id'],
                'closed_at': time.time(), 'app_bus_name_absent': True,
                'worker_active_while_closed': True, 'worker_nice': 10,
                'ready_announcement_absent_while_building': True,
                'controlled_build_delay_seconds': 90}
else:
    evidence = json.loads(args.output.read_text())
    candidate = json.loads((state / evidence['candidate'] / 'state.json').read_text())
    assert candidate['state'] == 'ready'
    claim = state / 'notifications' / (candidate['id'] + '-Ready')
    assert claim.is_file()
    if args.phase == 'completed':
        assert not app_owned()
        evidence['completed_at'] = time.time()
        evidence['app_bus_name_absent_at_completion'] = True
        evidence['ready_announcement_mtime'] = claim.stat().st_mtime
        evidence['ready_announcement_created_after_close'] = claim.stat().st_mtime > evidence['closed_at']
        assert evidence['ready_announcement_created_after_close']
    else:
        assert app_owned()
        assert claim.stat().st_mtime == evidence['ready_announcement_mtime']
        evidence['app_reopened'] = True
        evidence['ready_announcement_unchanged_after_reopen'] = True
args.output.write_text(json.dumps(evidence, indent=2) + '\n')
print(json.dumps(evidence, indent=2))
