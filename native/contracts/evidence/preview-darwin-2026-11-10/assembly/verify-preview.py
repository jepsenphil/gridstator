import contextlib
import hashlib
import http.client
import http.server
import json
import os
import pathlib
import re
import shutil
import signal
import socket
import ssl
import subprocess
import tarfile
import threading
import time
import urllib.parse

WORK = pathlib.Path(__file__).parent
RELEASE = pathlib.Path('/repos/gridstator/native/release/native-preview-20261110-cd6b769ebe6e')
EVIDENCE = WORK / 'verification'
EVIDENCE.mkdir(exist_ok=True)
RUNTIME = WORK / 'runtime'
RUNTIME.mkdir(exist_ok=True)
TOOLS = WORK / 'os-tools-only'
TOOLS.mkdir(exist_ok=True)
for name in ('sh', 'cat', 'curl', 'tar', 'gzip', 'awk', 'od', 'tr', 'mktemp', 'uname', 'wc', 'cp', 'chmod', 'mv', 'mkdir', 'rm', 'rmdir', 'sha256sum'):
    source = shutil.which(name, path='/usr/bin:/bin')
    assert source, name
    (TOOLS / name).symlink_to(source)
for name in ('node', 'npm', 'npx', 'rustc', 'cargo', 'rustup', 'python', 'python3', 'jq'):
    assert shutil.which(name, path=str(TOOLS)) is None, name

env = {k: v for k, v in os.environ.items() if not k.startswith(('GRIDSTATOR_', 'DOCKER_', 'CARGO_', 'RUSTUP_'))}
env.update(PATH=str(TOOLS), HOME=str(RUNTIME / 'home'), XDG_CONFIG_HOME=str(RUNTIME / 'config'), XDG_DATA_HOME=str(RUNTIME / 'data'), GRIDSTATOR_CONFIG=str(RUNTIME / 'client.json'), TMPDIR=str(RUNTIME / 'tmp'))
for name in ('home', 'config', 'data', 'tmp'):
    (RUNTIME / name).mkdir(exist_ok=True)

commands = []
def run(args, timeout=45, cwd=None):
    result = subprocess.run([str(a) for a in args], env=env, capture_output=True, text=True, timeout=timeout, cwd=cwd)
    commands.append({'argv': [str(a).replace(str(WORK), '<private>') for a in args], 'exitCode': result.returncode})
    if result.returncode:
        diagnostic = re.sub(r'(?im)^(Admin token:|.*token[:=]).*$', r'\1 <redacted>', result.stderr)
        raise AssertionError(f'{args[0]} exited {result.returncode}: {diagnostic[-2000:]}')
    return result

manifest_path = RELEASE / 'gridstator-release.json'
manifest_sha = hashlib.sha256(manifest_path.read_bytes()).hexdigest()
archive = RELEASE / 'gridstator-2.0.0-dev.1-linux-amd64.tar.gz'
manual = WORK / 'manual'
manual.mkdir(exist_ok=True)
with tarfile.open(archive, 'r:gz') as tar:
    members = tar.getmembers()
    entries = [m for m in members if m.name == 'gridstator']
    assert len(entries) == 1 and entries[0].isfile()
    assert {m.name for m in members} == {'gridstator', 'LICENSE'}
    with contextlib.closing(tar.extractfile(entries[0])) as source, (manual / 'gridstator').open('wb') as target:
        shutil.copyfileobj(source, target)
(manual / 'gridstator').chmod(0o755)
manual_result = run([manual / 'gridstator', 'release', 'verify', '--manifest', manifest_path, '--manifest-sha256', manifest_sha, '--archive', archive, '--executable', manual / 'gridstator', '--target', 'linux/amd64'])
manual_info = json.loads(run([manual / 'gridstator', 'build-info', '--json']).stdout)
assert manual_info['buildId'] == 'native-preview-20261110-cd6b769ebe6e'
assert manual_info['version'] == '2.0.0-dev.1'
assert manual_info['target'] == 'linux/amd64'
assert manual_info['sha256'] == hashlib.sha256((manual / 'gridstator').read_bytes()).hexdigest()
(EVIDENCE / 'manual-verification.json').write_text(manual_result.stdout)

requests = []
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        prefix = '/v2.0.0-dev.1/'
        name = self.path[len(prefix):] if self.path.startswith(prefix) else ''
        if not name or pathlib.PurePosixPath(name).name != name or not (RELEASE / name).is_file():
            self.send_error(404)
            return
        requests.append({'method': 'GET', 'path': self.path})
        self.send_response(200)
        self.send_header('Content-Length', str((RELEASE / name).stat().st_size))
        self.end_headers()
        with (RELEASE / name).open('rb') as source:
            shutil.copyfileobj(source, self.wfile)
    def log_message(self, *_):
        pass

server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
http_thread = threading.Thread(target=server.serve_forever, daemon=True)
http_thread.start()
controller = None
controller_log = None
result = None
try:
    prefix = WORK / 'installed prefix with spaces'
    install_args = ['/bin/sh', RELEASE / 'install-native.sh', '--tag', 'v2.0.0-dev.1', '--prefix', prefix, '--base-url', f'http://127.0.0.1:{server.server_port}']
    first = run(install_args, timeout=90)
    second = run(install_args, timeout=90)
    (EVIDENCE / 'installer.log').write_text(first.stdout + first.stderr + '\nRepeated identical install:\n' + second.stdout + second.stderr)
    installed = prefix / 'bin/gridstator'
    installed_info = json.loads(run([installed, 'build-info', '--json']).stdout)
    assert installed_info == manual_info
    assert run([installed, '--version']).stdout.strip() == '2.0.0-dev.1'
    metadata = prefix / 'share/gridstator/releases/v2.0.0-dev.1'
    assert hashlib.sha256((metadata / 'gridstator-release.json').read_bytes()).hexdigest() == manifest_sha
    assert json.loads((metadata / 'verification.json').read_text()) == json.loads(manual_result.stdout)
    assert not (prefix / 'share/gridstator/.installer-lock').exists()
    assert not list((RUNTIME / 'tmp').glob('gridstator-install.*'))
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 0))
        port = probe.getsockname()[1]
    data = RUNTIME / 'controller'
    # Captured init output contains a token. It is intentionally discarded and never copied to evidence.
    run([installed, 'controller', 'init', '--data-dir', data, '--host', '127.0.0.1', '--port', str(port)])
    assert (RUNTIME / 'client.json').is_file()
    controller_log = (RUNTIME / 'controller.log').open('w')
    controller = subprocess.Popen([str(installed), 'controller', 'start', '--data-dir', str(data)], env=env, stdout=controller_log, stderr=controller_log)
    deadline = time.monotonic() + 25
    status = None
    while time.monotonic() < deadline:
        assert controller.poll() is None, 'private controller exited before status'
        attempt = subprocess.run([str(installed), 'status', '--json'], env=env, capture_output=True, text=True, timeout=3)
        if attempt.returncode == 0:
            status = json.loads(attempt.stdout)
            break
        time.sleep(.1)
    assert status is not None, 'private TLS status deadline expired'
    project = RUNTIME / 'project'
    project.mkdir()
    payload = b'encoded digest preview read-back\n'
    (project / 'preview.txt').write_bytes(payload)
    run([installed, 'snapshot', '--json'], cwd=project)
    config = json.loads((RUNTIME / 'client.json').read_text())
    endpoint = urllib.parse.urlsplit(config['url'])
    digest = 'sha256:' + hashlib.sha256(payload).hexdigest()
    encoded_path = '/v1/blobs/' + urllib.parse.quote(digest, safe='')
    connection = http.client.HTTPSConnection(endpoint.hostname, endpoint.port, context=ssl.create_default_context(cadata=config['caPem']), timeout=5)
    try:
        connection.request('GET', encoded_path, headers={'Authorization': 'Bearer ' + config['token']})
        response = connection.getresponse()
        assert response.status == 200, response.status
        assert response.read() == payload
    finally:
        connection.close()
    controller.send_signal(signal.SIGTERM)
    stop_code = controller.wait(timeout=20)
    assert stop_code == 0, stop_code
    controller = None
    result = {
        'recordedAt': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
        'buildId': installed_info['buildId'], 'version': installed_info['version'],
        'target': installed_info['target'], 'manifestSha256': manifest_sha,
        'executable': {'sha256': installed_info['sha256'], 'size': installed_info['size']},
        'runtimePathPolicy': {'tools': sorted(p.name for p in TOOLS.iterdir()), 'absent': ['node','npm','npx','rustc','cargo','rustup','python','python3','jq'], 'scope': 'Installer, verifier, installed CLI and controller child processes. The private HTTP test server runs outside this PATH.'},
        'checks': {'manualArchiveRegularMember': True, 'manualNativeReleaseVerify': True, 'installerLoopbackDownload': True, 'repeatIdenticalInstall': True, 'prefixContainsSpaces': True, 'installedBytesMatchManualArchive': True, 'metadataRetained': True, 'installerLockAndTempCleaned': True, 'privateInit': True, 'actualPinnedTlsStatus': True, 'nativeSnapshotUpload': True, 'percentEncodedDigestTlsReadBack': {'path': encoded_path, 'bytes': len(payload), 'matchesExactUploadedBytes': True}, 'gracefulStopExitCode': stop_code, 'liveServiceOrGlobalPrefixChanged': False},
        'commands': commands,
        'httpRequests': requests,
        'limits': ['Linux amd64 installer/runtime smoke only; foreign target headers, sizes and digests are independently checked during assembly.', 'No workload, service registration, reboot or publication was performed.'],
        'cleanup': {'privateHttpStopped': True, 'controllerStopped': True, 'privateCredentialsAndDataRemoved': True}
    }
finally:
    if controller is not None and controller.poll() is None:
        controller.terminate()
        try:
            controller.wait(timeout=20)
        except subprocess.TimeoutExpired:
            controller.kill()
            controller.wait(timeout=5)
    if controller_log:
        controller_log.close()
    server.shutdown()
    server.server_close()
    http_thread.join(timeout=5)
    shutil.rmtree(RUNTIME)
if result:
    (EVIDENCE / 'installer-runtime-smoke.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({'result': 'passed', 'evidence': str(EVIDENCE), 'buildId': result['buildId'], 'httpRequests': len(requests), 'cleanup': result['cleanup']}))
