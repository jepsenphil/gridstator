import hashlib
import http.client
import json
import os
import pathlib
import re
import shutil
import signal
import socket
import ssl
import subprocess
import sys
import tarfile
import time
import urllib.parse

ROOT = pathlib.Path(__file__).parent
BINARY = ROOT / 'gridstator'
EXPECTED = 'native-preview-20261110-cd6b769ebe6e'
RUNTIME = ROOT / 'runtime'
EVIDENCE = ROOT / 'evidence'
RUNTIME.mkdir(mode=0o700)
EVIDENCE.mkdir()
for name in ('home', 'project', 'tmp', 'tools'):
    (RUNTIME / name).mkdir()
for name in ('sh', 'env', 'docker', 'act', 'git', 'tar', 'gzip', 'cat', 'uname'):
    path = shutil.which(name)
    assert path, name
    (RUNTIME / 'tools' / name).symlink_to(path)
for name in ('node', 'npm', 'cargo', 'rustc', 'python3', 'jq'):
    assert shutil.which(name, path=str(RUNTIME / 'tools')) is None
env = {key: value for key, value in os.environ.items() if not key.startswith(('GRIDSTATOR_', 'DOCKER_', 'GIT_'))}
env.update(HOME=str(RUNTIME / 'home'), PATH=str(RUNTIME / 'tools'), TMPDIR=str(RUNTIME / 'tmp'), GRIDSTATOR_CONFIG=str(RUNTIME / 'client.json'), GRIDSTATOR_CONTROLLER_DATA=str(RUNTIME / 'controller'), GRIDSTATOR_AGENT_DATA=str(RUNTIME / 'agent'), GRIDSTATOR_WSL_SECRET='private-wsl-fixture-must-be-masked')
commands = []
jobs = []
children = {}
logs = {}
secrets = [env['GRIDSTATOR_WSL_SECRET']]
image_tag = 'gridstator-wsl-validation:v00jpyv0'
build_id = None
node_id = None
client_config = None
windows_health_checks = []

def safe(text):
    for value in secrets:
        if value:
            text = text.replace(value, '<redacted>')
    return re.sub(r'(?im)^(Admin token:).*$', r'\1 <redacted>', text)

def cli(args, expected=0, timeout=120):
    result = subprocess.run([str(BINARY)] + args, cwd=RUNTIME / 'project', env=env, capture_output=True, text=True, timeout=timeout)
    redacted = list(args)
    for index, arg in enumerate(redacted[:-1]):
        if arg == '--token':
            redacted[index + 1] = '<redacted>'
    commands.append({'args': redacted, 'exitCode': result.returncode})
    assert result.returncode == expected, safe(f'CLI {redacted}: expected {expected}, got {result.returncode}: {result.stderr}')
    return result

def json_cli(args):
    return json.loads(cli(args).stdout)

def api(path):
    endpoint = urllib.parse.urlsplit(client_config['url'])
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    context.load_verify_locations(cadata=client_config['caPem'])
    connection = http.client.HTTPSConnection(endpoint.hostname, endpoint.port, context=context, timeout=5)
    try:
        connection.request('GET', path, headers={'Authorization': 'Bearer ' + client_config['token']})
        response = connection.getresponse()
        body = response.read()
        assert response.status == 200, safe(f'{path}: HTTP {response.status}: {body[:500]!r}')
        return json.loads(body)
    finally:
        connection.close()

def windows_health(stage):
    ca = '\\\\wsl.localhost\\Ubuntu' + str(RUNTIME / 'controller/pki/ca.pem').replace('/', '\\')
    command = ['/mnt/c/Windows/System32/curl.exe', '--fail', '--silent', '--show-error', '--max-time', '8', '--noproxy', '*', '--ssl-no-revoke', '--cacert', ca, client_config['url'] + '/v1/health']
    result = subprocess.run(command, env=env, capture_output=True, text=True, timeout=12)
    record = {'stage': stage, 'exitCode': result.returncode, 'url': client_config['url'] + '/v1/health', 'caTrust': 'Explicit private CA through owned WSL UNC path and hostname verification. Per-invocation revocation check disabled for this ephemeral private CA; global Windows trust unchanged.'}
    assert result.returncode == 0, safe('Windows native curl health: ' + result.stderr)
    record['health'] = json.loads(result.stdout)
    windows_health_checks.append(record)

def wait(predicate, timeout=75):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            value = predicate()
            if value:
                return value
        except Exception as error:
            last = error
        time.sleep(.2)
    raise AssertionError(safe(f'deadline expired: {last}'))

def start(role):
    output = (RUNTIME / f'{role}-{time.time_ns()}.log').open('w')
    logs.setdefault(role, []).append(output.name)
    child = subprocess.Popen([str(BINARY), role, 'start'], cwd=RUNTIME / 'project', env=env, stdin=subprocess.DEVNULL, stdout=output, stderr=output)
    output.close()
    children[role] = child
    return child.pid

def stop(role):
    child = children.pop(role, None)
    if child is None:
        return
    if child.poll() is None:
        child.send_signal(signal.SIGTERM)
        code = child.wait(timeout=35)
    else:
        code = child.returncode
    assert code == 0, f'{role} shutdown exit {code}'

def submit(args):
    job = json_cli(args + ['--json'])
    jobs.append(job['id'])
    return job

def terminal(job_id):
    def poll():
        job = api('/v1/jobs/' + job_id)
        return job if job['state'] in ('succeeded', 'failed', 'cancelled', 'lost', 'timed_out') else None
    return wait(poll, 150)

summary = None
try:
    info = json_cli(['build-info', '--json'])
    assert info['buildId'] == EXPECTED and info['target'] == 'linux/amd64'
    assert info['sha256'] == hashlib.sha256(BINARY.read_bytes()).hexdigest()
    assert info['sha256'] == 'eecbbc71e3c5a2234302fd637d5d668fc641935b32e878124f1b628a59473380'
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 0))
        port = probe.getsockname()[1]
    (RUNTIME / 'project/source.txt').write_text('wsl-native-source\n')
    (RUNTIME / 'project/.env').write_text('NEVER_UPLOAD_THIS=fixture\n')
    (RUNTIME / 'project/Dockerfile').write_text('FROM alpine:3.20\nCOPY source.txt /proof-source.txt\nCMD ["cat", "/proof-source.txt"]\n')
    # Initial admin output is deliberately discarded; no credentials enter evidence.
    cli(['controller', 'init', '--host', '127.0.0.1', '--port', str(port)])
    client_config = json.loads((RUNTIME / 'client.json').read_text())
    secrets.append(client_config['token'])
    controller_pid = start('controller')
    cluster = wait(lambda: api('/v1/cluster'))
    token = json_cli(['token', 'create', '--join', '--json'])['token']
    secrets.append(token)
    cli(['agent', 'join', client_config['url'], '--token', token, '--fingerprint', client_config['caFingerprint'], '--name', 'wsl-native-private-v00jpyv0'])
    saved = json.loads((RUNTIME / 'agent/agent.json').read_text())
    if saved.get('credential'):
        secrets.append(saved['credential'])
    worker_pid = start('agent')
    def online():
        nodes = api('/v1/nodes')['nodes']
        return next((node for node in nodes if node['status'] == 'online' and node.get('info', {}).get('docker', {}).get('available')), None)
    node = wait(online)
    node_id = node['id']
    windows_health('initial-controller-and-worker-ready')
    job = json_cli(['run', '--image', 'alpine:3.20', '--cpus', '1', '--memory', '128m', '--artifact', 'result.txt', '--secret-env', 'GRIDSTATOR_WSL_SECRET', '--json', '--', 'sh', '-c', 'cat source.txt; test ! -e .env; printf "secret=%s\\n" "$GRIDSTATOR_WSL_SECRET"; printf "wsl-artifact\\000\\377\\n" > result.txt'])
    jobs.append(job['id'])
    completed = terminal(job['id'])
    assert completed['state'] == 'succeeded', safe(str(completed))
    text = cli(['logs', job['id']]).stdout
    assert 'wsl-native-source' in text and 'secret=' in text
    assert env['GRIDSTATOR_WSL_SECRET'] not in text
    assert env['GRIDSTATOR_WSL_SECRET'] not in json.dumps(completed)
    (EVIDENCE / 'command-logs.txt').write_text(safe(text))
    artifacts = json_cli(['artifacts', job['id'], '--json'])
    artifacts = artifacts if isinstance(artifacts, list) else artifacts['artifacts']
    assert artifacts
    archive_path = RUNTIME / 'downloaded-artifact.tar'
    cli(['artifacts', job['id'], '--download', artifacts[0]['name'], '--out', str(archive_path)])
    with tarfile.open(archive_path, 'r:*') as archive:
        member = next(member for member in archive.getmembers() if pathlib.PurePosixPath(member.name).name == 'result.txt')
        assert archive.extractfile(member).read() == b'wsl-artifact\x00\xff\n'
    cli(['run', '--no-snapshot', '--image', 'alpine:3.20', '--cpus', '1', '--memory', '64m', '--', 'sh', '-c', 'exit 7'], expected=7)
    cancelled = json_cli(['run', '--no-snapshot', '--image', 'alpine:3.20', '--memory', '64m', '--json', '--', 'sleep', '60'])
    jobs.append(cancelled['id'])
    wait(lambda: api('/v1/jobs/' + cancelled['id'])['state'] == 'running')
    cli(['cancel', cancelled['id']])
    assert terminal(cancelled['id'])['state'] == 'cancelled'
    existing = subprocess.run(['docker', 'image', 'inspect', image_tag], env=env, capture_output=True)
    assert existing.returncode != 0, 'unique validation image tag unexpectedly exists'
    build = json_cli(['build', '--tag', image_tag, '--output', 'load', '--cpus', '1', '--memory', '128m', '--json'])
    build_id = build['id']
    jobs.append(build_id)
    built = terminal(build_id)
    assert built['state'] == 'succeeded', safe(str(built))
    image = json.loads(subprocess.run(['docker', 'image', 'inspect', image_tag], env=env, capture_output=True, text=True, check=True).stdout)[0]
    assert built['outputs']['image']['id'] == image['Id']
    assert built['outputs']['image']['sizeBytes'] > 0
    stop('agent')
    restarted_worker = start('agent')
    assert restarted_worker != worker_pid
    assert wait(online)['id'] == node_id
    windows_health('after-worker-process-restart')
    cli(['run', '--no-snapshot', '--image', 'alpine:3.20', '--memory', '64m', '--', 'echo', 'after-worker-restart'])
    stop('controller')
    restarted_controller = start('controller')
    assert restarted_controller != controller_pid
    wait(lambda: api('/v1/health'))
    assert wait(online)['id'] == node_id
    windows_health('after-controller-process-restart')
    output = cli(['run', '--no-snapshot', '--image', 'alpine:3.20', '--memory', '64m', '--', 'echo', 'after-controller-restart']).stdout
    assert 'after-controller-restart' in output
    summary = {'result': 'passed', 'recordedAt': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()), 'buildInfo': info, 'clusterId': cluster['clusterId'], 'nodeId': node_id, 'checks': {'nativeSnapshotBytesAndEnvExclusion': True, 'secretMaskingAndPublicSpecRedaction': True, 'artifactBinaryBytesExact': True, 'runExit7': True, 'runningCancellation': True, 'buildImageIdMatchesDockerInspect': True, 'buildImageSizePositive': True, 'workerRestartReusesNodeIdentity': True, 'controllerRestartReconnectsWorker': True, 'windowsNativeLoopbackTlsBeforeAndAfterRestarts': True}, 'windowsHealthChecks': windows_health_checks, 'imageId': image['Id'], 'jobIds': jobs, 'commands': commands, 'runtimePathAbsent': ['node','npm','cargo','rustc','python3','jq'], 'limits': 'Actual WSL2 x64 foreground CLI/controller/worker and Docker workflows. No WSL reboot/termination or host boot behavior tested; existing system v1 agent and Windows Code are separate.'}
finally:
    for role in ('agent', 'controller'):
        child = children.get(role)
        if child:
            try:
                stop(role)
            except Exception:
                if child.poll() is None:
                    child.kill()
                    child.wait(timeout=10)
    for role, paths in logs.items():
        (EVIDENCE / f'{role}.log').write_text(safe('\n'.join(pathlib.Path(path).read_text(errors='replace') for path in paths)))
    if build_id:
        inspected = subprocess.run(['docker', 'image', 'inspect', image_tag], env=env, capture_output=True, text=True)
        if inspected.returncode == 0:
            image = json.loads(inspected.stdout)[0]
            if image.get('Config', {}).get('Labels', {}).get('io.gridstator.job') == build_id:
                subprocess.run(['docker', 'image', 'rm', image_tag], env=env, capture_output=True, timeout=20, check=True)
    leftovers = []
    for job_id in jobs:
        found = subprocess.run(['docker', 'ps', '-aq', '--filter', 'label=io.gridstator.job=' + job_id], env=env, capture_output=True, text=True, check=True)
        leftovers.extend(found.stdout.split())
    if leftovers:
        subprocess.run(['docker', 'rm', '-f'] + leftovers, env=env, capture_output=True, timeout=30, check=True)
    shutil.rmtree(RUNTIME)
    if summary:
        summary['cleanup'] = {'privateRolesStopped': True, 'privateCredentialsAndDataRemoved': True, 'uniqueBuildTagRemoved': True, 'ownedContainerLeftoversRemoved': len(leftovers), 'globalPrefixOrExistingServiceChanged': False}
        (EVIDENCE / 'foreground-summary.json').write_text(json.dumps(summary, indent=2) + '\n')
if summary:
    print(json.dumps({'result': 'passed', 'buildId': EXPECTED, 'evidence': str(EVIDENCE), 'checks': summary['checks']}))
