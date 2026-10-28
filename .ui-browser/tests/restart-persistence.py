#!/usr/bin/env python3
"""Synthetic restart evidence. Pass an owned artifacts/r3-restart-*/ snapshot directory."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import uuid


root = Path(__file__).resolve().parents[2]
snapshot = Path(sys.argv[1]).absolute()
assert snapshot.parent == root / '.ui-browser/artifacts' and snapshot.name.startswith('r3-restart-')
assert snapshot.is_dir() and not snapshot.is_symlink()
public = snapshot / 'public'
manifest = json.loads((snapshot / 'snapshot.json').read_text())['sha256']
for relative, digest in manifest.items():
    path = public / relative
    assert not path.is_symlink() and path.is_file()
    assert hashlib.sha256(path.read_bytes()).hexdigest() == digest

run = snapshot / f'attempt-{uuid.uuid4().hex}'
run.mkdir(mode=0o700)
data = run / 'data'
handshake = run / 'handshake'
for path in (data, handshake):
    path.mkdir(mode=0o700)
    os.chown(path, 1000, 1000)
project = f'openproxy-ui-restart-{uuid.uuid4().hex}'
override = run / 'persistent-backend.compose.yaml'
override.write_text('''services:
  backend:
    command:
      - 'exec /runtime/openproxy --host 0.0.0.0 --port 4623 --data-dir /restart-data --web-dir /ui --no-open >/dev/null 2>&1'
    volumes:
      - type: bind
        source: ${OPENPROXY_UI_RESTART_DATA_DIR:?test-owned directory required}
        target: /restart-data
        bind:
          create_host_path: false
''')
env = {**os.environ, 'OPENPROXY_UI_WEB_DIR': str(public), 'OPENPROXY_UI_RESTART_DATA_DIR': str(data)}
compose = ['docker', 'compose', '--env-file', '/dev/null', '--project-name', project,
           '--project-directory', str(root / '.ui-browser'), '-f', str(root / '.ui-browser/compose.yaml'),
           '-f', str(root / '.ui-browser/tests/silent-backend.compose.yaml'), '-f', str(override)]


def command(args, *, capture=False, check=True):
    return subprocess.run(args, env=env, cwd=root, check=check, text=True,
                          stdout=subprocess.PIPE if capture else None,
                          stderr=subprocess.PIPE if capture else None)


def dc(*args, **kwargs):
    return command(compose + list(args), **kwargs)


def inspect(container):
    # Inspect only this disposable container; no production container discovery.
    return json.loads(command(['docker', 'inspect', container], capture=True).stdout)[0]


def state(container):
    item = inspect(container)
    return dict(id=item['Id'], pid=item['State']['Pid'], running=item['State']['Running'],
                startedAt=item['State']['StartedAt'], finishedAt=item['State']['FinishedAt'],
                exitCode=item['State']['ExitCode'], restartCount=item['RestartCount'])


def wait_healthy():
    dc('exec', '-T', 'backend', 'node', '--input-type=module', '-e', '''
const end = Date.now() + 20000;
while (true) {
  try { if ((await fetch('http://127.0.0.1:4623/health')).ok) break; } catch {}
  if (Date.now() > end) process.exit(1);
  await new Promise(resolve => setTimeout(resolve, 200));
}
''', capture=True)


def assert_quiet(container):
    item = inspect(container)
    assert item['HostConfig']['LogConfig']['Type'] == 'none'
    assert item['Config']['Entrypoint'] == ['/bin/sh', '-c']
    assert item['Config']['Cmd'][0].endswith('>/dev/null 2>&1')
    dc('exec', '-T', 'backend', '/bin/sh', '-c',
       'test "$(readlink /proc/1/fd/1)" = /dev/null && test "$(readlink /proc/1/fd/2)" = /dev/null', capture=True)


assert not dc('ps', '-aq', capture=True).stdout.strip()
assert not command(['docker', 'network', 'ls', '-q', '--filter', f'name=^{project}_isolated$'], capture=True).stdout.strip()
browser = None
created = False
evidence = {'project': project, 'snapshot': str(public), 'snapshotFiles': len(manifest)}
print(f'Synthetic restart project: {project}', flush=True)
try:
    config = json.loads(dc('config', '--format', 'json', capture=True).stdout)
    backend = config['services']['backend']
    assert backend['logging']['driver'] == 'none'
    assert backend['entrypoint'] == ['/bin/sh', '-c']
    assert backend['command'][0].endswith('>/dev/null 2>&1')
    assert config['networks']['isolated']['internal'] is True
    assert config['networks']['isolated']['driver_opts']['com.docker.network.bridge.gateway_mode_ipv4'] == 'isolated'
    for service in config['services'].values():
        assert not service.get('ports') and not service.get('privileged')
    created = True
    dc('create', '--no-build', '--pull', 'never', 'backend', 'app')
    container = dc('ps', '-aq', 'backend', capture=True).stdout.strip()
    item = inspect(container)
    assert item['HostConfig']['LogConfig']['Type'] == 'none'
    assert not item['HostConfig']['PortBindings']
    mounts = {mount['Destination']: mount for mount in item['Mounts']}
    assert mounts['/restart-data']['Source'] == str(data) and mounts['/restart-data']['Type'] == 'bind'
    assert mounts['/ui']['Source'] == str(public) and mounts['/ui']['RW'] is False
    dc('start', 'backend', 'app')
    wait_healthy()
    assert_quiet(container)  # Before the browser sends any credential.
    before = state(container)
    assert before['running'] and before['pid'] > 0
    evidence['quietBeforeCredentials'] = True
    browser_command = compose + ['run', '--rm', '--no-deps', '--pull', 'never', '-T',
        '-v', f'{root}/tests/dioxus_login_backend.browser.test.mjs:/test/dioxus_login_backend.browser.test.mjs:ro',
        '-v', f'{handshake}:/restart',
        '-e', 'RUN_DIOXUS_BACKEND_TEST=1', '-e', 'DIOXUS_BACKEND_OUTPUT_SUPPRESSED=1',
        '-e', 'DIOXUS_RESTART_HANDSHAKE_DIR=/restart',
        '-e', 'DASHBOARD_PLAYWRIGHT_MODULE=/opt/ui-browser/node_modules/playwright/index.mjs',
        '-e', 'DASHBOARD_CHROMIUM_EXECUTABLE=/opt/chrome-linux64/chrome',
        'browser', 'node', '--test', '--test-name-pattern=provider advertised context persists',
        '/test/dioxus_login_backend.browser.test.mjs']
    with (run / 'browser.tap').open('w') as output:
        browser = subprocess.Popen(browser_command, env=env, cwd=root, stdout=output, stderr=subprocess.STDOUT)
    deadline = time.monotonic() + 100
    while not (handshake / 'ready.json').exists():
        assert browser.poll() is None, 'Browser failed before requesting a backend restart'
        assert time.monotonic() < deadline, 'Browser restart readiness timed out'
        time.sleep(0.2)
    dc('stop', '--timeout', '20', 'backend')
    stopped = state(container)
    assert stopped['running'] is False and stopped['pid'] == 0
    dc('start', 'backend')
    wait_healthy()
    assert_quiet(container)
    after = state(container)
    assert after['running'] and after['pid'] > 0 and after['pid'] != before['pid']
    assert after['id'] == before['id'] and after['startedAt'] != before['startedAt']
    restart = {'before': before, 'stopped': stopped, 'after': after}
    evidence['restart'] = restart
    (handshake / 'restarted.tmp').write_text(json.dumps(restart))
    (handshake / 'restarted.tmp').rename(handshake / 'restarted.json')
    assert browser.wait(timeout=100) == 0, 'Browser persistence verification failed'
    evidence['verified'] = json.loads((handshake / 'verified.json').read_text())
    print(json.dumps({'actualProcessRestart': True, **restart}), flush=True)
finally:
    if browser is not None and browser.poll() is None:
        browser.terminate()
        browser.wait(timeout=20)
    if browser is not None:
        print((run / 'browser.tap').read_text(), flush=True)
    for name in ('ready', 'post-restart', 'verified'):
        path = handshake / f'{name}.json'
        if path.exists():
            evidence[name] = json.loads(path.read_text())
    if created:
        dc('down', '--remove-orphans')
    assert not dc('ps', '-aq', capture=True).stdout.strip()
    assert not command(['docker', 'network', 'ls', '-q', '--filter', f'name=^{project}_isolated$'], capture=True).stdout.strip()
    shutil.rmtree(data)  # Only the fresh directory created by this invocation.
    evidence['cleanup'] = {'containers': 'removed', 'network': 'removed', 'testData': 'removed'}
    (run / 'evidence.json').write_text(json.dumps(evidence, indent=2) + '\n')
    print(f'Evidence: {run / "evidence.json"}; disposable containers/network/data removed', flush=True)
