import pathlib,json,subprocess,ssl,urllib.request,os,time
base=pathlib.Path('/Users/philjepsen/gridstator-validation.I50ONpwm');root=base/'docker-proof-0d0cb012380c';proof=json.loads((root/'proof.json').read_text());cfg=json.loads((root/'client.json').read_text());env=os.environ.copy();env.update(json.loads((root/'environment.json').read_text())['env']);env['GRIDSTATOR_CONTROLLER_DATA']=str(root/'controller');env['GRIDSTATOR_AGENT_DATA']=str(root/'agent');env['GRIDSTATOR_CACHE']=str(root/'cache')
for k in ['GRIDSTATOR_URL','GRIDSTATOR_TOKEN','GRIDSTATOR_CA_FILE','GRIDSTATOR_SUPERVISED','GRIDSTATOR_INSTALL_ROOT','GRIDSTATOR_INSTALL_VERSION']:env.pop(k,None)
assert cfg['url'].startswith('https://127.0.0.1:') and pathlib.Path(env['GRIDSTATOR_CONFIG'])==root/'client.json';ctx=ssl.create_default_context(cadata=cfg['caPem']);docker=env['GRIDSTATOR_DOCKER'];binary=base/'payloads'/('m4-memory-dev-181bd9221ed7-arm64');nodeid=proof['nodeId'];expected=proof['clusterId'];events=[]
def rpc(method,path,data=None):
 request=urllib.request.Request(cfg['url']+path,data=None if data is None else json.dumps(data).encode(),headers={'Authorization':'Bearer '+cfg['token'],'Content-Type':'application/json'},method=method)
 with urllib.request.urlopen(request,context=ctx,timeout=10) as response:return json.load(response)
def node():return rpc('GET','/v1/nodes/'+nodeid)
def command(args,timeout=180):
 try:out=subprocess.run(args,env=env,cwd=root/'workspace with spaces',stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=timeout)
 except subprocess.TimeoutExpired:raise RuntimeError('controlled command timed out '+repr(args[:2])) from None
 if out.returncode:raise RuntimeError('controlled command failed '+repr(args[:2])+': '+out.stderr.decode())
 return out.stdout
assert rpc('GET','/v1/cluster')['clusterId']==expected
before=node();assert before['status']=='online' and before['info']['docker']['available'];assert not before['activeJobs'];assert command([docker,'ps','-q']).strip()==b'','Refuse Desktop stop while any container exists';os.kill(proof['workerPid'],0)
(root/'desktop-recovery-preflight.json').write_text(json.dumps({'node':before,'workerPid':proof['workerPid'],'noContainers':True},indent=2)+'\n')
try:
 command([docker,'desktop','stop','--timeout','60'],90);events.append({'stage':'desktopStopped','time':time.time()})
 for n in range(900):
  down=node()
  if down['status']=='online' and down['info']['docker']['available']==False and down['info']['executors']==[] and down['lastSeenAt']>before['lastSeenAt']:break
  time.sleep(.1)
 else:raise RuntimeError('worker never advertised unavailable Docker while online')
 assert down['connectedAt']==before['connectedAt'];os.kill(proof['workerPid'],0)
 queued=json.loads(command([str(binary),'run','--image','alpine:3.21','--cpus','1','--memory','128m','--json','--','sh','-c','printf desktop-recovered']))
 time.sleep(2);waiting=rpc('GET','/v1/jobs/'+queued['id']);assert waiting['state']=='queued',waiting['state'];heartbeat=node();assert heartbeat['status']=='online' and heartbeat['connectedAt']==before['connectedAt']
 events.append({'stage':'workerOnlineWithDockerUnavailable','node':down,'queuedJob':waiting,'time':time.time()})
finally:
 command([docker,'desktop','start','--timeout','120'],150);events.append({'stage':'desktopStarted','time':time.time()})
for n in range(1200):
 after=node();job=rpc('GET','/v1/jobs/'+queued['id'])
 if after['info']['docker']['available'] and 'command' in after['info']['executors'] and job['state']=='succeeded':break
 if job['state'] in ['failed','lost','cancelled']:raise RuntimeError('recovery job failed '+json.dumps(job))
 time.sleep(.1)
else:raise RuntimeError('Docker return did not restore admission and complete queued job')
assert after['status']=='online' and after['connectedAt']==before['connectedAt'];os.kill(proof['workerPid'],0);logs=rpc('GET','/v1/jobs/'+queued['id']+'/logs');stdout=''.join(e['d'] for e in logs['entries'] if e['o']=='stdout');assert stdout=='desktop-recovered';assert command([docker,'ps','-q']).strip()==b''
report={'result':'passed','scope':'temporary memory-overlay ARM release, actual Docker Desktop stop/start; no other containers existed; same worker PID and websocket connectedAt; heartbeats continued; queued work admitted after reprobe','build':proof['build'],'workerPid':proof['workerPid'],'nodeId':nodeid,'before':before,'down':down,'after':after,'queuedJob':waiting,'completedJob':job,'stdout':stdout,'events':events,'desktopLeftRunning':True};(root/'desktop-recovery.json').write_text(json.dumps(report,indent=2)+'\n');print(json.dumps({'result':'passed','workerPid':proof['workerPid'],'connectedAt':before['connectedAt'],'queuedJob':queued['id'],'root':str(root)}))
