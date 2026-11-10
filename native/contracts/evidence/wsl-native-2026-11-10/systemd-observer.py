import hashlib
import json
import os
import pathlib
import shlex
import signal
import subprocess
import time

ROOT = pathlib.Path(__file__).parent
EVIDENCE = ROOT / 'evidence'
TEMP = ROOT / 'service-tmp'
TEMP.mkdir(mode=0o700)
UNIT_ROOT = pathlib.Path.home() / '.config/systemd/user'
candidate_dirs = [UNIT_ROOT / 'default.target.wants', UNIT_ROOT, UNIT_ROOT.parent, UNIT_ROOT.parent.parent]
existed = {str(path): path.exists() for path in candidate_dirs}
before_units = {path.name for path in UNIT_ROOT.glob('gridstator-native-proof-*.service')}
env = {key: value for key, value in os.environ.items() if not key.startswith(('GRIDSTATOR_', 'DOCKER_'))}
env.update(GRIDSTATOR_TEST_BINARY=str(ROOT / 'gridstator'), TMPDIR=str(TEMP))
log = (EVIDENCE / 'systemd-test.log').open('w')
test = subprocess.Popen([str(ROOT / 'systemd-wsl-proof'), 'actual_systemd_supervisor_controller_and_embedded_worker_lifecycle', '--exact', '--ignored', '--nocapture', '--test-threads=1'], env=env, stdout=log, stderr=log)
handle = os.pidfd_open(test.pid)
paused = False
owned = set()
data_dir = None
seen = set()
health = []
initial_pid = None
restart_wait = False
restart_deadline = None
unit_adaptations = []

def pause_driver():
    global paused
    if not paused and test.poll() is None:
        signal.pidfd_send_signal(handle, signal.SIGSTOP)
        paused = True

def resume_driver():
    global paused
    if paused:
        signal.pidfd_send_signal(handle, signal.SIGCONT)
        paused = False

def windows_health(runtime):
    ca = '\\\\wsl.localhost\\Ubuntu' + str(data_dir / 'pki/ca.pem').replace('/', '\\')
    result = subprocess.run(['/mnt/c/Windows/System32/curl.exe', '--fail', '--silent', '--show-error', '--max-time', '5', '--noproxy', '*', '--ssl-no-revoke', '--cacert', ca, 'https://127.0.0.1:' + str(runtime['port']) + '/v1/health'], capture_output=True, text=True, timeout=8)
    if result.returncode != 0:
        return False
    health.append({'controllerPid': runtime['pid'], 'port': runtime['port'], 'health': json.loads(result.stdout), 'trust': 'Exact private CA and hostname verification; per-invocation revocation disabled for ephemeral private CA only. Global Windows trust unchanged.'})
    seen.add(runtime['pid'])
    return True

try:
    deadline = time.monotonic() + 150
    while test.poll() is None:
        assert time.monotonic() < deadline, 'unique user-service test deadline'
        if data_dir is None:
            for path in UNIT_ROOT.glob('gridstator-native-proof-*.service'):
                if path.name in before_units:
                    continue
                text = path.read_text()
                starts = [line.split('=', 1)[1] for line in text.splitlines() if line.startswith('ExecStart=')]
                if not starts:
                    continue
                args = shlex.split(starts[0])
                directory = pathlib.Path(args[args.index('--data-dir') + 1])
                assert directory.is_relative_to(TEMP), directory
                owned.add(path.name)
                data_dir = directory
                pause_driver()
                state = subprocess.run(['systemctl', '--user', 'show', path.name, '--property=ActiveState', '--value'], capture_output=True, text=True)
                assert state.stdout.strip() not in ('active', 'activating'), 'group adaptation must precede initial service start'
                shell = 'exec ' + shlex.join(args)
                escaped = shell.replace('\\', '\\\\').replace('"', '\\"').replace('$', '$$')
                replacement = 'ExecStart=/usr/bin/sg docker -c "' + escaped + '"'
                updated = text.replace('ExecStart=' + starts[0], replacement)
                assert updated != text
                assert 'KillMode=mixed' in updated
                updated = updated.replace('KillMode=mixed', 'KillMode=control-group')
                path.write_text(updated)
                subprocess.run(['systemctl', '--user', 'daemon-reload'], capture_output=True, check=True)
                unit_adaptations.append({'unit': path.name, 'scope': 'Owned test unit only; stale user manager lacks the account\'s already granted Docker group.', 'before': 'ExecStart=' + starts[0], 'after': replacement, 'killMode': {'before': 'mixed', 'after': 'control-group', 'reason': 'Extra sg launcher remains main process; signal all native actors for graceful stop.'}, 'mechanism': 'Noninteractive OS sg helper refreshes existing docker membership; no group files, sudo, Docker socket permissions or user-manager credentials changed.'})
                resume_driver()
                break
        if data_dir is not None:
            runtime_path = data_dir / 'runtime.json'
            if not runtime_path.exists() and initial_pid is not None and not restart_wait and len(seen) < 2:
                # Pause only the owned test driver; the real systemctl restart child and
                # systemd/native service keep running. This preserves time for the external
                # Windows health observation without adding a product delay or mocking OS calls.
                pause_driver()
                restart_wait = True
                restart_deadline = time.monotonic() + 25
            if runtime_path.exists():
                try:
                    runtime = json.loads(runtime_path.read_text())
                except (OSError, json.JSONDecodeError):
                    time.sleep(.005)
                    continue
                pid = runtime['pid']
                if pid not in seen:
                    pause_driver()
                    if windows_health(runtime):
                        if initial_pid is None:
                            initial_pid = pid
                        restart_wait = False
                        restart_deadline = None
                        resume_driver()
            if restart_wait:
                assert time.monotonic() < restart_deadline, 'service restart did not produce a ready runtime'
        time.sleep(.005 if initial_pid is not None else .02)
    assert test.returncode == 0, f'actual systemd contract failed with {test.returncode}'
    assert len(seen) >= 2, 'Windows native TLS health must cover initial and restarted service PIDs'
finally:
    resume_driver()
    if test.poll() is None:
        test.terminate()
        try:
            test.wait(timeout=10)
        except subprocess.TimeoutExpired:
            test.kill()
            test.wait(timeout=5)
    os.close(handle)
    log.close()
    for name in owned:
        subprocess.run(['systemctl', '--user', 'disable', '--now', name], capture_output=True)
        path = UNIT_ROOT / name
        if path.exists():
            text = path.read_text()
            assert str(TEMP) in text, 'refuse to remove unit outside owned namespace'
            path.unlink()
        subprocess.run(['systemctl', '--user', 'reset-failed', name], capture_output=True)
    subprocess.run(['systemctl', '--user', 'daemon-reload'], capture_output=True)
    for path in candidate_dirs:
        if not existed[str(path)]:
            try:
                path.rmdir()
            except (OSError, FileNotFoundError):
                pass

unit_check = subprocess.run(['systemctl', '--user', 'list-unit-files', 'gridstator-native-proof-*', '--no-legend', '--no-pager'], capture_output=True, text=True)
assert not any(name in unit_check.stdout for name in owned)
assert not list(TEMP.iterdir()), 'private service fixture must clean all credentials/data'
TEMP.rmdir()
summary = {'result': 'passed', 'recordedAt': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()), 'buildId': 'native-preview-20261110-cd6b769ebe6e', 'nativeSha256': hashlib.sha256((ROOT / 'gridstator').read_bytes()).hexdigest(), 'unitNames': sorted(owned), 'actualSystemdContract': 'actual_systemd_supervisor_controller_and_embedded_worker_lifecycle', 'windowsNativeTlsHealth': health, 'unitAdaptations': unit_adaptations, 'observationMethod': 'Only the owned test-driver process was paused briefly via pidfd while real systemd/native actors continued; no product timing or OS adapter calls were mocked.', 'checks': ['Actual unique user-systemd install/start and native supervisor/controller/embedded worker Docker job', 'Actual user-unit restart with changed payload PID', 'Windows native curl private-CA loopback health for initial and restarted service PIDs', 'Graceful stop, runtime removal, unit disable/uninstall and empty fixture cleanup'], 'limits': ['The existing library test uses its documented single-host managed-install fixture; this is not four-target release production proof.', 'The owned unit adds OS sg only to refresh existing Docker membership absent from the stale user manager; this does not claim the unadapted stale manager can access Docker.', 'User service only, with existing active system v1 agent untouched. No sudo password, linger enable, WSL termination/reboot or stopped-distro autostart claim.'], 'cleanup': {'privateFixtureEmptyAndRemoved': True, 'ownedUnitsRemoved': True, 'preexistingUnitNamesPreserved': sorted(before_units), 'preexistingDirectoriesNotRemoved': True}}
(EVIDENCE / 'systemd-summary.json').write_text(json.dumps(summary, indent=2) + '\n')
print(json.dumps({'result': 'passed', 'ownedUnits': sorted(owned), 'windowsHealthPids': sorted(seen), 'evidence': str(EVIDENCE)}))
