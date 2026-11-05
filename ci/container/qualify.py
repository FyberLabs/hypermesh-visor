#!/usr/bin/env python3
"""Qualify an already-started two-bot Compose project; no host UI/device access."""
import argparse
import concurrent.futures
import hashlib
import json
from pathlib import Path
import subprocess
import time

parser = argparse.ArgumentParser()
parser.add_argument('project')
parser.add_argument('--image', required=True)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
args.output.mkdir(parents=True, exist_ok=True)
root = Path(__file__).resolve().parents[2]
compose = ['docker', 'compose', '-p', args.project, '-f', str(root/'container/compose.yaml')]
import os
env = dict(os.environ, VISOR_IMAGE=args.image)

def run(argv, data=None, expected=0):
    result = subprocess.run(argv, input=data, capture_output=True, env=env)
    if result.returncode != expected:
        raise AssertionError(f'command failed ({result.returncode}): {result.stderr.decode(errors="replace")[-2000:]}')
    return result.stdout

def execute(bot, *argv, data=None, expected=0):
    return run(['docker', 'exec', '-i', f'{args.project}-{bot}-1', *argv], data, expected)

def script(bot, text):
    return execute(bot, 'python3', '-c', text)

def ready(bot):
    start = time.monotonic()
    while time.monotonic() - start < 45:
        info = json.loads(run(['docker', 'inspect', f'{args.project}-{bot}-1']))[0]
        if info['State'].get('Health', {}).get('Status') == 'healthy':
            return time.monotonic() - start
        assert info['State']['Running'], 'container exited'
        time.sleep(0.2)
    raise AssertionError('container not healthy')

report = {'image': args.image, 'bots': {}, 'checks': []}
def passed(label):
    report['checks'].append(label)
    print('PASS ' + label, flush=True)

for bot in ('writer', 'reader'):
    readiness = ready(bot)
    inspection = json.loads(run(['docker', 'inspect', f'{args.project}-{bot}-1']))[0]
    host = inspection['HostConfig']
    assert inspection['Config']['User'] == '1000:1000'
    assert host['ReadonlyRootfs'] and host['NetworkMode'] == 'none' and not host['Privileged']
    assert not host['Devices'] and not host['PortBindings']
    assert 'ALL' in host['CapDrop'] and 'no-new-privileges:true' in host['SecurityOpt']
    assert not host.get('Binds') or not any('/var/run/docker.sock' in b for b in host['Binds'])
    peer_home = f'{args.project}_' + ('reader-home' if bot == 'writer' else 'writer-home')
    assert all(m.get('Name') != peer_home for m in inspection['Mounts'])
    identity = script(bot, "from pathlib import Path; print(Path('/home/bot/.local/state/hypermesh/bot-id').read_text().strip())").decode().strip()
    report['bots'][bot] = {'id': identity, 'image_id': inspection['Image'], 'readiness_seconds': readiness}
    execute(bot, 'mkdir', '-p', '/home/bot/qualification')
    run(['docker', 'cp', str(root/'ci/display')+'/.', f'{args.project}-{bot}-1:/home/bot/qualification/'])
    script(bot, "from pathlib import Path; assert not Path('/dev/uinput').exists(); assert not Path('/var/run/docker.sock').exists(); Path('/home/bot/private-marker').write_text('private-' + '"+bot+"')")
assert report['bots']['writer']['id'] != report['bots']['reader']['id']
passed('nonroot private-mounts no-host-devices no-host-socket no-ports distinct-persistent-identities')

result = execute('writer', 'visor-resource', 'put', 'share', 'message', '--expected', 'missing', data=b'shared fixture')
assert json.loads(result)['sha256'] == hashlib.sha256(b'shared fixture').hexdigest()
assert execute('reader', 'visor-resource', 'get', 'share', 'message') == b'shared fixture'
execute('reader', 'visor-resource', 'put', 'share', 'denied', '--expected', 'missing', data=b'no', expected=1)
execute('writer', 'visor-resource', 'get', 'share', '../private-marker', expected=2)
script('writer', "import os; os.symlink('/home/bot/private-marker', '/resources/share/escape')")
execute('reader', 'visor-resource', 'get', 'share', 'escape', expected=1)
script('writer', "from pathlib import Path; Path('/resources/share/escape').unlink()")
script('writer', "import os; os.link('/resources/share/message', '/resources/share/hardlink')")
execute('reader', 'visor-resource', 'get', 'share', 'hardlink', expected=1)
script('writer', "from pathlib import Path; Path('/resources/share/hardlink').unlink()")

def publish(number):
    return subprocess.run(['docker','exec','-i',f'{args.project}-writer-1','visor-resource','put','share','race','--expected','missing'], input=str(number).encode(), capture_output=True)
with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
    outcomes = list(pool.map(publish, (1, 2)))
assert sorted(r.returncode for r in outcomes) == [0, 1], 'CAS did not choose one writer'
assert execute('reader', 'visor-resource', 'get', 'share', 'race') in (b'1', b'2')
passed('shared-read write-grants ro-denial traversal-symlink-hardlink-refusal atomic-conflict')
script('writer', """import errno
from pathlib import Path
path=Path('/resources/share/quota-probe')
written=0
try:
    with path.open('wb', buffering=0) as stream:
        while written <= 268435456:
            written += stream.write(b'0'*65536)
except OSError as error:
    assert error.errno == errno.ENOSPC
else:
    raise AssertionError('shared mount has no hard quota')
finally:
    path.unlink(missing_ok=True)
assert written <= 268435456
""")
passed('shared-filesystem-hard-quota cleanup')

# Same discovery/HTTP fixture in both independent namespaces.
for bot in ('writer', 'reader'):
    for scenario in ('mcp-source-precedence', 'mcp-http', 'mcp-discovery'):
        output = execute(bot, 'python3', f'/home/bot/qualification/{scenario}.py', '/run/visor/hypermesh', '/home/bot/.config/hypermesh', '/home/bot/workspace')
        (args.output/f'{bot}-{scenario}.log').write_bytes(output)
        assert b'PASS ' in output
passed('two-independent-bots discovery trust identity overrides http-sse lifecycle')

# Container restart retains private identity/trust but never shares the peer home.
reader_id = report['bots']['reader']['id']
reader_pid = json.loads(run(['docker','inspect',f'{args.project}-reader-1']))[0]['State']['Pid']
for phase in ('prepare','unchanged','changed','revoked'):
    if phase != 'prepare':
        run(compose+['restart','writer'])
        ready('writer')
    output = execute('writer','python3','/home/bot/qualification/mcp-persistence.py',phase,
                     '/run/visor/hypermesh','/home/bot/.config/hypermesh','/home/bot/workspace')
    with (args.output/'writer-persistence.log').open('ab') as stream:
        stream.write(output)
    assert script('writer',"from pathlib import Path; print(Path('/home/bot/.local/state/hypermesh/bot-id').read_text().strip())").decode().strip() == report['bots']['writer']['id']
    assert json.loads(run(['docker','inspect',f'{args.project}-reader-1']))[0]['State']['Pid'] == reader_pid
    assert script('reader',"from pathlib import Path; print(Path('/home/bot/.local/state/hypermesh/bot-id').read_text().strip())").decode().strip() == reader_id
execute('writer','python3','/home/bot/qualification/mcp-persistence.py','cleanup','/run/visor/hypermesh','/home/bot/.config/hypermesh','/home/bot/workspace')
passed('restart-isolation durable-trust changed-source-denial revocation stable-identity')

report['packages'] = execute('writer','cat','/usr/share/visor-packages.txt').decode()
report['image'] = json.loads(run(['docker','image','inspect',args.image]))[0]
# Keep only reproducible metadata, not engine paths or environment/config values.
report['image'] = {k: report['image'][k] for k in ('Id','Architecture','Os','Size','RepoDigests')}
(args.output/'report.json').write_text(json.dumps(report, indent=2)+'\n')
print('container qualification passed; project remains available for inspection', flush=True)
