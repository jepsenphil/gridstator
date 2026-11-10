import os,json,hashlib,tarfile,shutil,subprocess
from pathlib import Path
r=Path('/Users/philjepsen/gridstator-validation.I50ONpwm/mixed-cd6-20261110-31df')
p=r/'package';raw=(p/'gridstator-release.json').read_bytes();m=json.loads(raw);a=m['artifacts']['darwin/arm64'];archive=p/a['archive']['file']
assert hashlib.sha256(archive.read_bytes()).hexdigest()==a['archive']['sha256']
s=r/'install';v=s/'versions'/m['buildId'];v.mkdir(parents=True,mode=0o700,exist_ok=False)
with tarfile.open(archive,'r:gz') as t:
 es=[e for e in t.getmembers() if e.name==a['executable']['file']];assert len(es)==1 and es[0].isfile()
 with t.extractfile(es[0]) as src,open(v/'gridstator','xb') as out:shutil.copyfileobj(src,out)
exe=v/'gridstator';exe.chmod(0o700);assert hashlib.sha256(exe.read_bytes()).hexdigest()==a['executable']['sha256']
h=hashlib.sha256(raw).hexdigest()
verified=subprocess.run([str(exe),'release','verify','--manifest',str(p/'gridstator-release.json'),'--manifest-sha256',h,'--target','darwin/arm64','--archive',str(archive),'--executable',str(exe)],text=True,capture_output=True)
assert verified.returncode==0,verified.stderr
(r/'baseline-native-verification.json').write_text(verified.stdout)
(v/'release.json').write_bytes(raw)
(v/'installed.json').write_text(json.dumps({'buildId':m['buildId'],'target':'darwin/arm64','executable':str(exe),'manifestSha256':h},indent=2)+'\n')
(s/'state.json').write_text(json.dumps({'schemaVersion':1,'current':m['buildId'],'previous':None,'pending':None,'lastRollback':None},indent=2)+'\n')
shutil.copy2(exe,s/'supervisor');(s/'supervisor').chmod(0o700)
for f in [v/'release.json',v/'installed.json',s/'state.json']:f.chmod(0o600)
(r/'docker-config'/'config.json').write_text(json.dumps({'cliPluginsExtraDirs':['/Applications/Docker.app/Contents/Resources/cli-plugins']})+'\n');(r/'docker-config'/'config.json').chmod(0o600)
env=os.environ.copy();env.update({'DOCKER_CONFIG':str(r/'docker-config'),'DOCKER_HOST':'unix:///Users/philjepsen/.docker/run/docker.sock','PATH':'/Applications/Docker.app/Contents/Resources/bin:'+env.get('PATH','/usr/bin:/bin')})
x=subprocess.run(['/Applications/Docker.app/Contents/Resources/bin/docker','info','--format','{{json .}}'],env=env,text=True,capture_output=True);assert x.returncode==0,x.stderr
info=json.loads(x.stdout)
(r/'docker-ready.json').write_text(json.dumps({k:info.get(k) for k in ['ID','ServerVersion','Architecture','OSType','NCPU','MemTotal']},indent=2)+'\n')
print(json.dumps({'baselineBuildId':m['buildId'],'target':'darwin/arm64','executableSha256':a['executable']['sha256'],'archiveSha256':a['archive']['sha256'],'nativeVerificationPassed':True,'privateDockerReady':True}))
