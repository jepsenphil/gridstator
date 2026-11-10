import pathlib,json,subprocess,os,signal,time,shutil,ssl,urllib.request,sys,hashlib
root=pathlib.Path('/home/pj/gridstator-validation-v00jpyv0');gui=root/'gui-proof';sys.path.insert(0,str(gui));import window_guard as g
fixture=json.loads((gui/'fixture.json').read_text());proof=json.loads((gui/'evidence/full-smoke.json').read_text());cfg=json.loads((gui/'client.json').read_text());ctx=ssl.create_default_context(cadata=cfg['caPem'])
def rpc(path):
 request=urllib.request.Request(cfg['url']+path,headers={'Authorization':'Bearer '+cfg['token']})
 with urllib.request.urlopen(request,context=ctx,timeout=10) as reply:return json.load(reply)
assert rpc('/v1/cluster')['clusterId']==fixture['clusterId']
for n in range(200):
 node=rpc('/v1/nodes/'+fixture['nodeId'])
 if not node['activeJobs']:break
 time.sleep(.1)
assert not node['activeJobs']
for args in [['ps','-aq'],['volume','ls','-q']]:assert subprocess.check_output(['/usr/bin/docker']+args+['--filter','label=io.gridstator.node='+fixture['nodeId']]).strip()==b''
for pid,role,child in [(fixture['workerPid'],'agent','backend-agent'),(fixture['controllerPid'],'controller','backend-controller')]:
 proc=pathlib.Path('/proc')/str(pid);assert (proc/'exe').resolve()==gui/'gridstator';argv=(proc/'cmdline').read_bytes().split(b'\0');assert role.encode() in argv and str(gui/child).encode() in argv;os.kill(pid,signal.SIGTERM)
for n in range(150):
 alive=[pid for pid in [fixture['workerPid'],fixture['controllerPid']] if pathlib.Path('/proc',str(pid)).exists()]
 if not alive:break
 time.sleep(.1)
assert not alive,alive
owned_server=[]
for proc in pathlib.Path('/proc').iterdir():
 if not proc.name.isdigit():continue
 try:exe=(proc/'exe').resolve();owned=(root/'server') in exe.parents
 except (FileNotFoundError,PermissionError,ProcessLookupError):continue
 if owned:
  os.kill(int(proc.name),signal.SIGTERM);owned_server.append(int(proc.name))
q=lambda value:"'"+value.replace("'","''")+"'"
script="$prefix="+q(g.WIN_NATIVE+'\\')+";$owned=@(Get-Process -ErrorAction SilentlyContinue|Where-Object {$_.Path -and $_.Path.StartsWith($prefix,[System.StringComparison]::OrdinalIgnoreCase)});$ids=@($owned|Select-Object -ExpandProperty Id);$owned|Stop-Process;[pscustomobject]@{stoppedOwnedIds=$ids;stockIds=@(Get-Process Code -ErrorAction SilentlyContinue|Where-Object {$_.Path -eq 'C:\\Program Files\\Microsoft VS Code\\Code.exe'}|Select-Object -ExpandProperty Id);resolverPipes=@([System.IO.Directory]::GetFiles('\\\\.\\pipe\\')|Where-Object {$_ -like '*vscode-wsl-*'})}|ConvertTo-Json -Compress"
windows=g.powershell(script)
prep=json.loads((root/'private-code-preparation.json').read_text());assert sorted(windows['stockIds'])==sorted(x['Id'] for x in prep['processesBefore']);assert not windows['resolverPipes'];assert not(pathlib.Path.home()/'.vscode-server').exists()
legacy={'status':subprocess.check_output(['systemctl','show','gridstator-agent.service','-p','MainPID','-p','ActiveState','-p','SubState'],text=True),'unitSha256':hashlib.sha256(pathlib.Path('/etc/systemd/system/gridstator-agent.service').read_bytes()).hexdigest()};assert 'MainPID=618566' in legacy['status'] and 'ActiveState=active' in legacy['status'];assert legacy['unitSha256']=='c2450846820fd06a0e4199995fe1ea9735b7c3ffe2ff2a34e44dadd0dcf1658c'
original=pathlib.Path('/mnt/c/Program Files/Microsoft VS Code/Code.exe');assert hashlib.sha256(original.read_bytes()).hexdigest()==prep['originalCodeExeSha256'];report={'result':'passed','ownNativeActorsExited':[fixture['workerPid'],fixture['controllerPid']],'ownLinuxServerProcessesStopped':owned_server,'windows':windows,'stock23DesktopPidSetUnchanged':True,'stockCodeExeSha256Unchanged':True,'legacySystemAgent':legacy,'ownedDockerContainersAndVolumesAbsent':True,'noDefaultWslServerCreated':True,'removedWindowsPrivateRoot':str(g.WIN),'removedWslGuiRoot':str(root),'globalSharedStorageWasNotReset':True,'historicalDiagnosticWritesNotRuledOut':True,'scope':'only owned fixture processes/paths; no global service, WSL restart, engine restart, global editor settings or global storage reset'}
# Preserve report outside both deletion roots, with no credentials.
report_path=pathlib.Path('/tmp/gridstator-wsl-v00jpyv0-cleanup.json');report_path.write_text(json.dumps(report,indent=2)+'\n')
for n in range(10):
 try:shutil.rmtree(g.WIN);break
 except PermissionError:
  if n==9:raise
  time.sleep(1)
shutil.rmtree(root);report['privateWindowsRootAbsent']=not g.WIN.exists();report['privateWslRootAbsent']=not root.exists();report_path.write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report))
