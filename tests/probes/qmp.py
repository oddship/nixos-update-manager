#!/usr/bin/env python3
"""Operate only the disposable QEMU guest monitor; never the host desktop."""
import argparse
import json
from pathlib import Path
import socket
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--monitor', type=Path)
action = parser.add_mutually_exclusive_group(required=True)
action.add_argument('--screenshot', type=Path)
action.add_argument('--key', help='QEMU sendkey names, e.g. ctrl-l or ret')
action.add_argument('--text', help='Type ASCII text into the disposable guest')
action.add_argument('--click', nargs=2, type=int, metavar=('X', 'Y'))
action.add_argument('--powerdown', action='store_true')
action.add_argument('--scroll', type=int, help='Scroll at the current pointer position; positive is down')
parser.add_argument('--width', type=int, default=1280)
parser.add_argument('--height', type=int, default=800)
args = parser.parse_args()
root = Path(__file__).resolve().parents[2]
monitor = args.monitor or Path((root / 'artifacts/gnome-runtime/monitor-directory').read_text().strip()) / 'qmp'
with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
    connection.settimeout(10)
    connection.connect(str(monitor))
    stream = connection.makefile('rwb')
    json.loads(stream.readline())
    def execute(name, arguments=None):
        stream.write((json.dumps({'execute': name, 'arguments': arguments or {}}) + '\n').encode())
        stream.flush()
        while True:
            response = json.loads(stream.readline())
            if 'error' in response:
                raise RuntimeError(response['error'])
            if 'return' in response:
                return response['return']
    execute('qmp_capabilities')
    def send_key(key, hold=None):
        result = execute('human-monitor-command', {'command-line': 'sendkey ' + key + (' ' + str(hold) if hold else '')})
        if result:
            raise RuntimeError(result)
    if args.screenshot:
        output = args.screenshot.resolve()
        output.parent.mkdir(parents=True, exist_ok=True)
        execute('screendump', {'filename': str(output)})
        print(output)
    elif args.key:
        send_key(args.key)
    elif args.text:
        punctuation = {'/': 'slash', '-': 'minus', '_': 'shift-minus', '.': 'dot',
                       ' ': 'spc', ':': 'shift-semicolon', '@': 'shift-2',
                       '>': 'shift-dot', '&': 'shift-7', '=': 'equal',
                       '[': 'bracket_left', ']': 'bracket_right', ',': 'comma',
                       '"': 'shift-apostrophe', "'": 'apostrophe', chr(92): 'backslash',
                       '(': 'shift-9', ')': 'shift-0', ';': 'semicolon'}
        keys = []
        for character in args.text:
            if character in punctuation:
                keys.append(punctuation[character])
            elif character.isascii() and character.isalnum():
                keys.append(('shift-' if character.isupper() else '') + character.lower())
            else:
                raise ValueError('Unsupported character for guest typing: ' + repr(character))
        for key in keys:
            send_key(key, 30)
            time.sleep(0.12)
    elif args.click:
        x, y = args.click
        execute('input-send-event', {'events': [
            {'type': 'abs', 'data': {'axis': 'x', 'value': int(x * 32767 / args.width)}},
            {'type': 'abs', 'data': {'axis': 'y', 'value': int(y * 32767 / args.height)}}]})
        execute('input-send-event', {'events': [{'type': 'btn', 'data': {'down': True, 'button': 'left'}}]})
        time.sleep(0.08)
        execute('input-send-event', {'events': [{'type': 'btn', 'data': {'down': False, 'button': 'left'}}]})
    elif args.scroll is not None:
        if not 1 <= abs(args.scroll) <= 50:
            raise ValueError('Scroll count must be between 1 and 50')
        button = 'wheel-down' if args.scroll > 0 else 'wheel-up'
        for _ in range(abs(args.scroll)):
            execute('input-send-event', {'events': [{'type': 'btn', 'data': {'down': True, 'button': button}}, {'type': 'btn', 'data': {'down': False, 'button': button}}]})
            time.sleep(0.08)
    else:
        execute('system_powerdown')
