#!/usr/bin/env python3
"""No CLI argument or environment switch grants desktop automation authority."""
import json
import os
import subprocess
import sys

executable = sys.argv[1]
for arguments in ([], ['--bridge'], ['--unknown'], ['--permissions', '--bridge']):
    for override in ('0', '1'):
        result = subprocess.run([executable, *arguments],
            input='\n'.join(json.dumps(request) for request in [
                {'action': 'view'}, {'action': 'keyboard', 'strokes': [{'keysym': 97, 'down': True}]},
                {'action': 'pointer', 'steps': [{'kind': 'button', 'button': 0, 'down': True}]}
            ]) + '\n', text=True, capture_output=True, timeout=5,
            env=dict(os.environ, HYPERMESH_NATIVE_BRIDGE=override))
        assert result.returncode == 1, (arguments, result.returncode)
        assert json.loads(result.stdout) == {'error': 'Native automation disabled pending authenticated daemon integration.'}
print('PASS unauthenticated bridge refused for capture, keyboard and pointer requests')
