#!/usr/bin/env python3
"""Read-only production baseline: all native/reference writes stay in fresh owned temp directories."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[4]
BINARY_SHA = '8cf0f97be1fcfae1b9d1a6a601ccb7f600298bb9ca59f862c6369351f4c6aa60'
NATIVE_SOURCE_SHA = '7f406a9b5a2a6eba65474b4440a421af1dd27f40837314e2be97a870586cd21e'
REFERENCE_SHA = '0f73cc2686ec82d460ffbe09e7f487098bceb9a532490e93b8de099b5b6067df'


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def mode(path):
    return format(stat.S_IMODE(path.stat().st_mode), '04o')


def isolated_environment(root, config):
    for name in ['home', 'config', 'data', 'cache', 'run', 'scratch']:
        (root / name).mkdir(mode=0o700)
    return {'PATH': os.environ.get('PATH', '/usr/bin:/bin'), 'LANG': 'C.UTF-8',
            'HOME': str(root / 'home'), 'XDG_CONFIG_HOME': str(root / 'config'),
            'XDG_DATA_HOME': str(root / 'data'), 'XDG_CACHE_HOME': str(root / 'cache'),
            'XDG_RUNTIME_DIR': str(root / 'run'), 'TMPDIR': str(root / 'scratch'),
            'TMP': str(root / 'scratch'), 'TEMP': str(root / 'scratch'),
            'GRIDSTATOR_CONFIG': config, 'GRIDSTATOR_CONTROLLER_DATA': str(root / 'controller'),
            'GRIDSTATOR_AGENT_DATA': str(root / 'agent'), 'GRIDSTATOR_CACHE': str(root / 'cache')}


def run(args, env, cwd):
    return subprocess.run(args, env=env, cwd=cwd, text=True, capture_output=True, timeout=30,
                          preexec_fn=lambda: os.umask(0o022))


def native_command(binary, root):
    return [str(binary), 'controller', 'init', '--data-dir', str(root / 'controller'),
            '--host', '127.0.0.1', '--port', '0']


def reference_command(filename):
    js = 'import {writeFileAtomic} from ' + json.dumps((HERE / 'frozen-v1-fs.mjs').as_uri())
    js += '; await writeFileAtomic(process.argv[1], JSON.stringify({marker:"reference-v1"}));'
    return ['node', '--input-type=module', '--eval', js, filename]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path,
                        default=REPO / 'native/self-build/proof-01M72202QV3TGHQEHTC67VN3HJ/linux-amd64/gridstator')
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    assert sha(binary) == BINARY_SHA, 'Only immutable7c x64 baseline bytes are accepted'
    assert sha(HERE / 'native-state-7c41.rs') == NATIVE_SOURCE_SHA
    assert sha(HERE / 'frozen-v1-fs.ts') == REFERENCE_SHA
    receipt = {'buildId': 'native-preview-20261110-7c41b2f3c79d', 'binarySha256': BINARY_SHA,
               'source': {'nativeStateSha256': NATIVE_SOURCE_SHA, 'frozenV1Revision': '4b8f13ccf1b21820007fef0ab52143c961b69216',
                          'frozenV1FsSha256': REFERENCE_SHA, 'compiledReferenceHelperSha256': sha(HERE / 'frozen-v1-fs.mjs')},
               'uid': os.getuid(), 'umask': '0022', 'nodeVersion': subprocess.check_output(['node', '--version'], text=True).strip(),
               'scope': 'Every config/data/home/cache/runtime/temp write is inside a fresh owned TemporaryDirectory. No actual /tmp or user-directory permission target. No services/network listeners started.',
               'cases': [], 'productionEdits': False, 'artifactsRebuiltOrModified': False}
    for relative in [False, True]:
        with tempfile.TemporaryDirectory(prefix='gridstator-config-parent-baseline-') as td:
            root = Path(td)
            shared = root / 'existing-shared-native'
            shared.mkdir()
            shared.chmod(0o775)
            reference = root / 'existing-shared-reference'
            reference.mkdir()
            reference.chmod(0o775)
            env = isolated_environment(root, 'client.json' if relative else str(shared / 'client.json'))
            command = native_command(binary, root)
            before = mode(shared)
            native = run(command, env, shared)
            public = {'case': 'relative-leaf-config' if relative else 'absolute-custom-config',
                      'nativeCommand': command, 'nativeWorkingDirectory': str(shared), 'isolatedEnvironment': env,
                      'native': {'exitCode': native.returncode, 'parentBefore': before, 'parentAfter': mode(shared),
                                 'clientFileExists': (shared / 'client.json').exists(),
                                 'clientFileMode': mode(shared / 'client.json') if (shared / 'client.json').exists() else None,
                                 'dedicatedDataMode': mode(root / 'controller'), 'rawConfigAndSuccessOutputOmitted': True}}
            if native.returncode:
                public['native']['error'] = re.sub(r'gs[acj]_[A-Za-z0-9_-]+', '<redacted>', native.stderr)
            ref_path = 'reference.json' if relative else str(reference / 'client.json')
            ref_cwd = shared if relative else reference
            ref_before = mode(ref_cwd)
            ref_command = reference_command(ref_path)
            result = run(ref_command, env, ref_cwd)
            assert result.returncode == 0, result.stderr
            ref_file = ref_cwd / ref_path
            public['referenceCommand'] = ref_command
            public['referenceWorkingDirectory'] = str(ref_cwd)
            public['reference'] = {'exitCode': result.returncode, 'parentBefore': ref_before,
                                   'parentAfter': mode(ref_cwd), 'fileMode': mode(ref_file)}
            if relative:
                assert native.returncode == 1 and not (shared / 'client.json').exists()
                assert mode(shared) == '0775' and 'No such file or directory' in native.stderr
            else:
                assert native.returncode == 0 and mode(shared) == '0700' and mode(shared / 'client.json') == '0600'
            assert public['reference']['parentBefore'] == public['reference']['parentAfter'] == '0775'
            assert public['reference']['fileMode'] == '0600' and public['native']['dedicatedDataMode'] == '0700'
        assert not root.exists()
        public['temporaryDirectoryRemoved'] = True
        receipt['cases'].append(public)
    receipt['result'] = 'Both historical defects reproduced against immutable7c; frozenv1 preserves parent mode and accepts relative leaf'
    text = json.dumps(receipt, indent=2) + '\n'
    assert not re.search(r'gs[acj]_[A-Za-z0-9_-]+|-----BEGIN (?:RSA |EC )?PRIVATE KEY-----', text)
    if args.output:
        args.output.write_text(text)
    print(text, end='')


if __name__ == '__main__':
    main()
