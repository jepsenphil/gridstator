import pathlib,json,subprocess,os,socket,time,ssl,urllib.request,tarfile,io,uuid
base=pathlib.Path('/Users/philjepsen/gridstator-validation.I50ONpwm');root=base/('docker-proof-'+uuid.uuid4().hex[:12]);root.mkdir(mode=0o700);workspace=root/'workspace with spaces';workspace.mkdir();(workspace/'gridstator.yml').write_text('version: 1\nproject: isolated-m4-docker-proof\n');(workspace/'hello.txt').write_text('from-m4-snapshot\n');(workspace/'Dockerfile').write_text('FROM scratch\nCOPY hello.txt /hello.txt\n')
binary=base/'payloads/native-preview-20261110-cd6b769ebe6e-arm64';docker='/Applications/Docker.app/Contents/Resources/bin/docker';env=os.environ.copy();env.update({'PATH':str(base/'docker-tools/bin')+':/Applications/Docker.app/Contents/Resources/bin:/usr/bin:/bin:/usr/sbin:/sbin','DOCKER_HOST':'unix:///Users/philjepsen/.docker/run/docker.sock','GRIDSTATOR_DOCKER':docker,'GRIDSTATOR_ACT':str(base/'docker-tools/bin/act'),'GRIDSTATOR_CONFIG':str(root/'client.json'),'GRIDSTATOR_CONTROLLER_DATA':str(root/'controller'),'GRIDSTATOR_AGENT_DATA':str(root/'agent'),'GRIDSTATOR_CACHE':str(root/'cache')})
for key in ['GRIDSTATOR_URL','GRIDSTATOR_TOKEN','GRIDSTATOR_CA_FILE','GRIDSTATOR_SUPERVISED','GRIDSTATOR_INSTALL_ROOT','GRIDSTATOR_INSTALL_VERSION']:env.pop(key,None)
info=json.loads(subprocess.check_output([str(binary),'build-info','--json'],env=env));assert info['buildId']=='native-preview-20261110-cd6b769ebe6e'
def cli(args):
 try:result=subprocess.run([str(binary)]+args,env=env,cwd=workspace,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=180)
 except subprocess.TimeoutExpired:raise RuntimeError('private CLI timeout '+repr(args[:2])) from None
 if result.returncode:raise RuntimeError('private CLI failed '+repr(args[:2])+': '+result.stderr.decode())
 return result.stdout
listener=socket.socket();listener.bind(('127.0.0.1',0));port=listener.getsockname()[1];listener.close();cli(['controller','init','--host','127.0.0.1','--port',str(port),'--data-dir',str(root/'controller')]);cfg=json.loads((root/'client.json').read_text());expected=json.loads((root/'controller/controller.json').read_text())['clusterId'];assert cfg['url']=='https://127.0.0.1:'+str(port);ctx=ssl.create_default_context(cadata=cfg['caPem'])
def launch(args,name):
 log=(root/name).open('wb');return subprocess.Popen([str(binary)]+args,env=env,cwd=workspace,stdin=subprocess.DEVNULL,stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
controller=launch(['controller','start','--data-dir',str(root/'controller')],'controller.log')
def rpc(method,path,data=None,raw=False):
 request=urllib.request.Request(cfg['url']+path,data=None if data is None else json.dumps(data).encode(),headers={'Authorization':'Bearer '+cfg['token'],'Content-Type':'application/json'},method=method)
 with urllib.request.urlopen(request,context=ctx,timeout=10) as reply:return reply.read() if raw else json.load(reply)
for n in range(200):
 try:
  cluster=rpc('GET','/v1/cluster');assert cluster['clusterId']==expected;break
 except Exception:time.sleep(.1)
else:raise RuntimeError('private controller readiness')
rpc('PATCH','/v1/settings',{'autoUpdate':'off','toolchain':{'act':'off'}});join=rpc('POST','/v1/tokens',{'kind':'join','name':'actual-m4-docker-proof','maxUses':1});cli(['agent','join',cfg['url'],'--token',join['token'],'--fingerprint',cfg['caFingerprint'],'--name',root.name,'--data-dir',str(root/'agent')]);nodeid=json.loads((root/'agent/agent.json').read_text())['nodeId'];worker=launch(['agent','start','--data-dir',str(root/'agent')],'worker.log')
def node():return rpc('GET','/v1/nodes/'+nodeid)
for n in range(600):
 value=node()
 if value['status']=='online' and value['info']['docker']['available'] and 'command' in value['info']['executors']:break
 time.sleep(.1)
else:raise RuntimeError('native Docker worker readiness')
daemon=json.loads(subprocess.check_output([docker,'info','--format','{{json .}}'],env=env));cap=value['info']['executionCapacity'];assert cap['cpus']==daemon['NCPU'];assert cap['memoryTotalMb']==daemon['MemTotal']//1024//1024;assert value['info']['cpus']==value['info']['host']['cpus'];assert value['info']['memoryTotalMb']==value['info']['host']['memoryTotalMb'];assert value['allocatable']['cpus']<=min(value['info']['cpus'],cap['cpus']);assert value['allocatable']['memoryMb']<=min(value['info']['memoryTotalMb'],cap['memoryTotalMb']);assert cap['memoryTotalMb']<value['info']['memoryTotalMb']
def terminal(jobid,wanted):
 for n in range(1200):
  current=rpc('GET','/v1/jobs/'+jobid)
  if current['state']==wanted:return current
  if current['state'] in ['failed','lost','cancelled','succeeded'] and current['state']!=wanted:raise RuntimeError('private job terminal mismatch '+json.dumps(current))
  time.sleep(.1)
 raise RuntimeError('private job timeout '+jobid)
command=json.loads(cli(['run','--image','alpine:3.21','--cpus','1','--memory','128m','--artifact','result.txt','--json','--','sh','-c','cat hello.txt; uname -s; printf m4-docker-artifact > result.txt']));done=terminal(command['id'],'succeeded');snapshot=rpc('GET','/v1/snapshots/'+done['snapshotId']+'?manifest=true');hello=next(entry for entry in snapshot['manifest']['entries'] if entry['path']=='hello.txt');encodedBlob=rpc('GET','/v1/blobs/'+hello['digest'].replace(':','%3A'),raw=True);assert encodedBlob==b'from-m4-snapshot\n';logs=rpc('GET','/v1/jobs/'+done['id']+'/logs');stdout=''.join(entry['d'] for entry in logs['entries'] if entry['o']=='stdout');assert 'from-m4-snapshot' in stdout and 'Linux' in stdout;archive=rpc('GET','/v1/jobs/'+done['id']+'/artifacts/result.txt.tar',raw=True)
with tarfile.open(fileobj=io.BytesIO(archive)) as tar:contents=b''.join(tar.extractfile(entry).read() for entry in tar if entry.isfile())
assert contents==b'm4-docker-artifact'
slow=json.loads(cli(['run','--image','alpine:3.21','--cpus','1','--memory','64m','--json','--','sleep','300']))
for n in range(300):
 current=rpc('GET','/v1/jobs/'+slow['id'])
 if current['state']=='running':break
 time.sleep(.1)
else:raise RuntimeError('cancel job never running')
cli(['cancel',slow['id']]);cancelled=terminal(slow['id'],'cancelled')
tag='gridstator-m4-proof/'+root.name+':load';build=json.loads(cli(['build','--output','load','--tag',tag,'--cpus','1','--memory','128m','--json']));built=terminal(build['id'],'succeeded');inspect=json.loads(subprocess.check_output([docker,'image','inspect',tag],env=env))[0];assert built['outputs']['image']['id']==inspect['Id'];assert built['outputs']['image']['sizeBytes']==inspect['Size'];subprocess.run([docker,'image','rm',tag],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE,check=True)
report={'result':'passed','root':str(root),'build':info,'controllerPid':controller.pid,'workerPid':worker.pid,'nodeId':nodeid,'clusterId':expected,'daemon':{k:daemon[k] for k in ['ServerVersion','NCPU','MemTotal','Architecture','OSType','OperatingSystem']},'readyNode':value,'command':{'id':done['id'],'state':done['state'],'stdout':stdout,'artifactBytes':contents.decode(),'tarBytes':len(archive)},'cancelled':{'id':cancelled['id'],'state':cancelled['state']},'dockerBuild':{'id':built['id'],'state':built['state'],'outputs':built['outputs'],'inspectId':inspect['Id'],'inspectSize':inspect['Size'],'ownedTagRemoved':tag},'encodedBlobGet':{'digest':hello['digest'],'path':'/v1/blobs/'+hello['digest'].replace(':','%3A'),'exactBytes':encodedBlob.decode()},'scope':'actual final common release Mac CLI/controller/standalone worker against Docker Desktop Linux ARM VM; snapshot,command,artifact,cancel,load image metadata and host-vs-VM capacity'};(root/'proof.json').write_text(json.dumps(report,indent=2)+'\n');(root/'environment.json').write_text(json.dumps({'env':{k:v for k,v in env.items() if k in ['PATH','DOCKER_HOST','GRIDSTATOR_DOCKER','GRIDSTATOR_ACT','GRIDSTATOR_CONFIG']},'root':str(root)}));os.chmod(root/'environment.json',0o600);print(json.dumps({'root':str(root),'result':'passed','controllerPid':controller.pid,'workerPid':worker.pid,'nodeId':nodeid}))
