#!/usr/bin/env python3
"""Exercise exported-package discovery without building or activating anything."""
import json
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
with tempfile.TemporaryDirectory(prefix='nixos-updates-exports-') as temporary:
    root = Path(temporary)
    tools = root / 'tools'
    tools.mkdir()
    (tools / 'flake.nix').write_text('''{ outputs = { self }: let
      package = n: builtins.derivation { name = "demo${toString n}-1.1.0"; system = "x86_64-linux"; builder = "/bin/sh"; };
    in { packages.x86_64-linux = builtins.listToAttrs (builtins.genList (i: { name = "demo${toString i}"; value = package i; }) 10) // {
      default = package 0;
      broken = throw "a broken export must not hide working packages";
      notPackage = 1;
    }; }; }''')
    (root / 'flake.nix').write_text('{ inputs.tools.url = "path:' + str(tools) + '"; outputs = { ... }: {}; }')
    subprocess.run(['nix', 'flake', 'lock', str(root)], check=True, capture_output=True)
    expression = 'import ' + str(ROOT / 'src/input-packages.nix') + ' { flake = ' + json.dumps('path:' + str(root)) + '; inputs = [ "tools" ]; system = "x86_64-linux"; }'
    records = json.loads(subprocess.check_output(['nix', 'eval', '--impure', '--json', '--expr', expression], text=True))
    assert len(records) == 11, records
    assert len({record['path'] for record in records}) == 10
    assert all(record['input'] == 'tools' and record['version'] == '1.1.0' for record in records)
    assert all(record['attribute'] not in ('broken', 'notPackage') for record in records)
    print('Ten exported packages, alias deduplication data, and partial evaluation checked; no outputs built.')
