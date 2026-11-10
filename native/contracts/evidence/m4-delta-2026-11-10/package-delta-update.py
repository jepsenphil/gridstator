import hashlib, json, pathlib, shutil, struct, subprocess, sys, tarfile
root = pathlib.Path(sys.argv[1])
packages = root / 'worker-update-delta-c36d79523a20-packages'
packages.mkdir(exist_ok=True)
for name, payload in [('rehearsal-baseline', 'm4-final-arm64'), ('rehearsal-confirmed', 'm4-final-c36d79523a20-arm64')]:
    binary = root / 'payloads' / payload
    info = json.loads(subprocess.check_output([str(binary), 'build-info', '--json']))
    directory = packages / name
    directory.mkdir(exist_ok=True)
    shutil.copy2(binary, packages / (name + '-gridstator'))
    artifacts = {}
    for target in ['linux/amd64', 'linux/arm64', 'darwin/amd64', 'darwin/arm64']:
        if target == 'darwin/arm64':
            executable = binary
        else:
            header = bytearray(64)
            if target.startswith('linux/'):
                header[:6] = bytes([0x7f, ord('E'), ord('L'), ord('F'), 2, 1])
                header[18:20] = struct.pack('<H', 62 if target.endswith('amd64') else 183)
            else:
                header[:4] = bytes([0xcf, 0xfa, 0xed, 0xfe])
                header[4:8] = struct.pack('<I', 0x01000007 if target.endswith('amd64') else 0x0100000c)
            executable = directory / ('NOT-RUNTIME-PROOF-' + target.replace('/', '-'))
            executable.write_bytes(header)
        archive = directory / ('gridstator-' + info['buildId'] + '-' + target.replace('/', '-') + '.tar.gz')
        with tarfile.open(archive, 'w:gz', compresslevel=1) as tar:
            entry = tarfile.TarInfo('gridstator')
            entry.mode = 0o755
            entry.size = executable.stat().st_size
            with executable.open('rb') as source:
                tar.addfile(entry, source)
        def digest(path, name):
            return {'file': name, 'size': path.stat().st_size, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}
        artifacts[target] = {'archive': digest(archive, archive.name), 'executable': digest(executable, 'gridstator')}
    manifest = {'schemaVersion': 1, 'buildId': info['buildId'], 'version': info['version'], 'sourceRevision': info['sourceRevision'], 'protocol': {'api': 1, 'agent': 1, 'update': 1}, 'artifacts': artifacts}
    (directory / 'gridstator-release.json').write_text(json.dumps(manifest, indent=2) + '\n')
    print(name, info['buildId'], info['target'])
