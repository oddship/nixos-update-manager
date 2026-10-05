#!/usr/bin/env python3
"""Synthetic Pi JSON engine for disposable-guest UI checks; never a provider."""
import json
import os
from pathlib import Path
import sys

flags = sys.argv[1:]
if '--version' in flags:
    print('synthetic-pi-fixture')
    sys.exit(0)
for flag in ('--no-tools', '--no-extensions', '--no-skills', '--no-prompt-templates',
             '--no-context-files', '--no-session', '--no-approve', '--print', '--offline'):
    assert flag in flags, flag
assert flags[flags.index('--mode') + 1] == 'json'
assert 'NIXOS_UPDATES_HELPER' not in os.environ
prompt = sys.stdin.read()
is_test = prompt == 'This is a connection test. Reply only with Connected.'
model = flags[flags.index('--model') + 1] if '--model' in flags else ''
context = None if is_test else json.loads(prompt.split('\n', 1)[1])
with Path('/tmp/xchg/pi-requests.jsonl').open('a') as log:
    log.write(json.dumps({'synthetic_provider': True, 'connection_test': is_test,
                          'model': model, 'cwd': os.getcwd(), 'context': context}) + '\n')
if model == 'fixture/fail':
    sys.exit('Synthetic connection failure requested by the fixture.')
answer = 'Connected' if is_test else 'Fixture explanation: ten packages are added at version 1.1.0.'
print(json.dumps({'type': 'message_end', 'message': {'role': 'assistant',
                  'stopReason': 'stop', 'content': [{'type': 'text', 'text': answer}]}}))
print(json.dumps({'type': 'agent_settled'}))
