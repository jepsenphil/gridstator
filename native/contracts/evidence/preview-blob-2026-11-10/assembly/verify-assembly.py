import hashlib
import json
import pathlib
import struct
import tarfile
import time

ROOT = pathlib.Path('/repos/gridstator')
WORK = pathlib.Path(__file__).parent
OUT = ROOT / 'native/release/native-preview-20261110-e3e5fae51c8e'

def digest(path):
    result = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            result.update(chunk)
    return result.hexdigest()

manifest = json.loads((OUT / 'gridstator-release.json').read_text())
checks = []
for target, artifact in manifest['artifacts'].items():
    path = OUT / artifact['archive']['file']
    assert path.stat().st_size == artifact['archive']['size']
    assert digest(path) == artifact['archive']['sha256']
    with tarfile.open(path, 'r:gz') as archive:
        members = archive.getmembers()
        assert [member.name for member in members] == ['gridstator', 'LICENSE']
        assert all(member.isfile() for member in members)
        executable = archive.getmember('gridstator')
        assert executable.mode == 0o755
        assert executable.size == artifact['executable']['size']
        actual = hashlib.sha256()
        header = b''
        size = 0
        with archive.extractfile(executable) as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b''):
                if not header:
                    header = chunk[:65536]
                size += len(chunk)
                actual.update(chunk)
        assert size == artifact['executable']['size']
        assert actual.hexdigest() == artifact['executable']['sha256']
        assert hashlib.sha256(archive.extractfile('LICENSE').read()).hexdigest() == digest(ROOT / 'LICENSE')
    info = json.loads((OUT / 'target-build-info' / f"{target.replace('/', '-')}.json").read_text())
    for key in ('buildId', 'version', 'sourceRevision'):
        assert info[key] == manifest[key], (target, key)
    assert info['sha256'] == artifact['executable']['sha256']
    assert info['size'] == size and info['target'] == target
    if target.startswith('linux'):
        assert header[:6] == b'\x7fELF\x02\x01'
        assert struct.unpack_from('<H', header, 18)[0] == (62 if target.endswith('amd64') else 183)
        offset = struct.unpack_from('<Q', header, 32)[0]
        entry_size, count = struct.unpack_from('<HH', header, 54)
        segments = [struct.unpack_from('<IIQQQQQQ', header, offset + index * entry_size) for index in range(count)]
        assert all(segment[0] != 3 for segment in segments), 'PT_INTERP'
        for segment in segments:
            if segment[0] == 2:
                original = WORK / 'input' / target.replace('/', '-') / 'gridstator'
                with original.open('rb') as source:
                    source.seek(segment[2])
                    table = source.read(segment[5])
                assert all(struct.unpack_from('<qQ', table, index)[0] != 1 for index in range(0, len(table), 16)), 'DT_NEEDED'
        header_scope = 'ELF64 correct machine; no PT_INTERP or DT_NEEDED'
    else:
        assert header[:4] == bytes.fromhex('cffaedfe')
        assert struct.unpack_from('<I', header, 4)[0] == (0x01000007 if target.endswith('amd64') else 0x0100000c)
        header_scope = 'thin Mach-O 64-bit correct CPU'
    checks.append({'target': target, 'archive': artifact['archive'], 'executable': artifact['executable'], 'headerAndLinkage': header_scope, 'buildInfoMatches': True, 'licenseMatches': True})

bundle = OUT / 'gridstator-2.0.0-dev.1-native-bundle.tar.gz'
with tarfile.open(bundle, 'r:gz') as archive:
    expected = ['gridstator-release.json'] + [artifact['archive']['file'] for artifact in manifest['artifacts'].values()]
    assert {member.name for member in archive.getmembers()} == set(expected)
    assert all(member.isfile() for member in archive.getmembers())
    for name in expected:
        result = hashlib.sha256()
        with archive.extractfile(name) as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b''):
                result.update(chunk)
        assert result.hexdigest() == digest(OUT / name), name

checkpoint = json.loads((OUT / 'SOURCE_CHECKPOINT.json').read_text())
for item in checkpoint['files']:
    assert digest(ROOT / item['path']) == item['sha256'], item['path']
supplement = json.loads((OUT / 'SUPPLEMENTAL_BUILD_INPUTS.json').read_text())
supplement_files = supplement.get('files') or supplement.get('supplementalBuildInputs', [])
assert len(supplement_files) == 3
for item in supplement_files:
    assert digest(ROOT / item['path']) == item['sha256'], item['path']
assert digest(OUT / 'gridstator-native.vsix') == 'd0cafff141d2ca5e318a0bc0633150f2ce896e356a399bea92b44dec7ba35eb6'
record = {'recordedAt': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()), 'result': 'passed', 'buildId': manifest['buildId'], 'version': manifest['version'], 'parentSourceRevision': manifest['sourceRevision'], 'productionInputHashesVerified': len(checkpoint['files']), 'supplementalInputHashesVerified': len(supplement_files), 'manifestSha256': digest(OUT / 'gridstator-release.json'), 'fourTargets': checks, 'nativeBundleMembersMatchAdjacentFiles': True, 'vsixSha256': digest(OUT / 'gridstator-native.vsix'), 'limits': 'Header/linkage/identity verification for all four; runtime evidence is per-target in PREVIEW.json and is not inferred from headers.'}
(OUT / 'verification/assembly-verification.json').write_text(json.dumps(record, indent=2) + '\n')
files = sorted(path for path in OUT.rglob('*') if path.is_file() and path != OUT / 'SHA256SUMS')
(OUT / 'SHA256SUMS').write_text(''.join(f'{digest(path)}  {path.relative_to(OUT).as_posix()}\n' for path in files))
print(json.dumps({'filesChecksummed': len(files), 'manifestSha256': record['manifestSha256'], 'bundleSha256': digest(bundle), 'checksumFileSha256': digest(OUT / 'SHA256SUMS'), 'fourTargetVerification': 'passed', 'sourceInputsStillMatch': True}))
