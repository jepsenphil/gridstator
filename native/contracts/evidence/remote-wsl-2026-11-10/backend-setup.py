import pathlib,json,hashlib,subprocess,os,socket,ssl,urllib.request,time,zipfile
root=pathlib.Path('/home/pj/gridstator-validation-v00jpyv0/gui-proof');binary=root/'gridstator';assert hashlib.sha256(binary.read_bytes()).hexdigest()=='eecbbc71e3c5a2234302fd637d5d668fc641935b32e878124f1b628a59473380';binary.chmod(0o555);package=root/'gridstator-native.vsix';assert hashlib.sha256(package.read_bytes()).hexdigest()=='d0cafff141d2ca5e318a0bc0633150f2ce896e356a399bea92b44dec7ba35eb6'
assert not(root/'backend-controller').exists(),'fresh private backend only';extension=root/'extension';extension.mkdir()
with zipfile.ZipFile(package) as archive:
 for name in archive.namelist():
  if name.startswith('extension/') and not name.endswith('/'):
   target=extension/name[len('extension/'):];assert '..' not in target.relative_to(extension).parts;target.parent.mkdir(parents=True,exist_ok=True);target.write_bytes(archive.read(name))
env=os.environ.copy();env.update({'GRIDSTATOR_CONFIG':str(root/'client.json'),'GRIDSTATOR_CONTROLLER_DATA':str(root/'backend-controller'),'GRIDSTATOR_AGENT_DATA':str(root/'backend-agent'),'GRIDSTATOR_CACHE':str(root/'cache'),'GRIDSTATOR_DOCKER':'/usr/bin/docker','GRIDSTATOR_ACT':'/usr/local/bin/act'})
for key in ['GRIDSTATOR_URL','GRIDSTATOR_TOKEN','GRIDSTATOR_CA_FILE','GRIDSTATOR_SUPERVISED','GRIDSTATOR_INSTALL_ROOT','GRIDSTATOR_INSTALL_VERSION']:env.pop(key,None)
def cli(args):
 try:reply=subprocess.run([str(binary)]+args,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=90)
 except subprocess.TimeoutExpired:raise RuntimeError('private CLI timeout '+repr(args[:2])) from None
 if reply.returncode:raise RuntimeError('private CLI failed '+repr(args[:2])+': '+reply.stderr.decode())
 return reply.stdout
info=json.loads(cli(['build-info','--json']));assert info['buildId']=='native-preview-20261110-cd6b769ebe6e' and info['target']=='linux/amd64'
sock=socket.socket();sock.bind(('127.0.0.1',0));port=sock.getsockname()[1];sock.close();assert port!=7300;cli(['controller','init','--host','127.0.0.1','--port',str(port),'--data-dir',str(root/'backend-controller')]);cfg=json.loads((root/'client.json').read_text());assert cfg['url']=='https://127.0.0.1:'+str(port);expected=json.loads((root/'backend-controller/controller.json').read_text())['clusterId'];ctx=ssl.create_default_context(cadata=cfg['caPem'])
def launch(args,name):
 log=(root/'evidence'/name).open('wb');return subprocess.Popen([str(binary)]+args,env=env,stdout=log,stderr=subprocess.STDOUT,stdin=subprocess.DEVNULL,start_new_session=True)
controller=launch(['controller','start','--data-dir',str(root/'backend-controller')],'controller.log')
def rpc(method,path,data=None):
 request=urllib.request.Request(cfg['url']+path,data=None if data is None else json.dumps(data).encode(),headers={'Authorization':'Bearer '+cfg['token'],'Content-Type':'application/json'},method=method)
 with urllib.request.urlopen(request,context=ctx,timeout=10) as response:return json.load(response)
for attempt in range(200):
 try:cluster=rpc('GET','/v1/cluster');assert cluster['clusterId']==expected;break
 except Exception:time.sleep(.1)
else:raise RuntimeError('private GUI controller readiness')
rpc('PATCH','/v1/settings',{'autoUpdate':'off','toolchain':{'act':'off'}});token=rpc('POST','/v1/tokens',{'kind':'join','name':'actual-wsl-gui-proof','maxUses':1});cli(['agent','join',cfg['url'],'--token',token['token'],'--fingerprint',cfg['caFingerprint'],'--name','private-actual-wsl-editor','--data-dir',str(root/'backend-agent')]);nodeid=json.loads((root/'backend-agent/agent.json').read_text())['nodeId'];worker=launch(['agent','start','--data-dir',str(root/'backend-agent')],'worker.log')
for attempt in range(600):
 node=rpc('GET','/v1/nodes/'+nodeid)
 if node['status']=='online' and node['info']['docker']['available'] and 'command' in node['info']['executors']:break
 time.sleep(.1)
else:raise RuntimeError('private GUI Docker worker readiness')
yaml='''version: 1
project: actual-wsl-native-smoke
defaults:
  image: alpine:3.21
  resources:
    cpus: 1
    memory: 128m
tasks:
  proof:
    command: |
      set -eu
      cat 'payload with spaces.txt'
      mkdir -p reports
      printf '%s\\n' 'remote-native-artifact' > reports/result.txt
      printf '%s\\n' '<testsuites><testsuite name="remote" tests="1"><testcase name="actual WSL snapshot" classname="remote" file="source.js" line="1"/></testsuite></testsuites>' > reports/junit.xml
    artifacts: [reports/result.txt]
    junit: reports/junit.xml
  slow:
    command: echo remote-running; sleep 300
'''
fixture={'root':str(root),'workspace':str(root/'workspace with spaces'),'config':str(root/'client.json'),'cli':str(binary),'controllerData':str(root/'local-admin-hints'),'agentData':str(root/'agent-unused'),'cache':str(root/'cache'),'url':cfg['url'],'remotePort':port,'clusterId':expected,'nodeId':nodeid,'nodeName':'private-actual-wsl-editor','nativeBuildId':info['buildId'],'nativeSha256':info['sha256'],'caFingerprint':cfg['caFingerprint'],'packageSha256':hashlib.sha256(package.read_bytes()).hexdigest(),'projectYaml':yaml,'controllerPid':controller.pid,'workerPid':worker.pid};(root/'tests/smoke-fixture.json').write_text(json.dumps(fixture,indent=2)+'\n');(root/'fixture.json').write_text(json.dumps(fixture,indent=2)+'\n');print(json.dumps({'result':'ready','clusterId':expected,'nodeId':nodeid,'url':cfg['url'],'controllerPid':controller.pid,'workerPid':worker.pid,'native':info}))
