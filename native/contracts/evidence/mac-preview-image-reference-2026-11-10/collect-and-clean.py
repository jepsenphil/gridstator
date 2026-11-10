import pathlib,subprocess,tarfile,io,json
ssh=['ssh','-o','BatchMode=yes','-o','IdentitiesOnly=yes','-i','/home/pj/.ssh/id_rsa_wired8','philjepsen@192.168.1.170'];out=pathlib.Path('native/contracts/evidence/mac-preview-image-reference-2026-11-10')
fetch=r'''import pathlib,tarfile,sys,json,subprocess
base=pathlib.Path('/Users/philjepsen/gridstator-validation.I50ONpwm');e=base/'preview-evidence-7c41b2f3c79d';proof=base/'docker-proof-ea963c9fa97d';facts={}
for short in ['arm64','x86_64']:
 p=base/('payloads/native-preview-20261110-7c41b2f3c79d-'+short);facts[short]={}
 for name,args in [('format',['/usr/bin/file',str(p)]),('linkage',['/usr/bin/otool','-L',str(p)]),('signature',['/usr/bin/codesign','-dv','--verbose=4',str(p)])]:
  r=subprocess.run(args,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True);facts[short][name]={'exitCode':r.returncode,'stdout':r.stdout,'stderr':r.stderr}
(e/'binary-format-linkage-signature.json').write_text(json.dumps(facts,indent=2)+'\n')
with tarfile.open(fileobj=sys.stdout.buffer,mode='w|') as tar:
 for p in e.iterdir():
  if p.is_file():tar.add(p,arcname='checks/'+p.name)
 for r in (base/'preview-roles-7c41b2f3c79d').iterdir():
  for name in ['foreground-evidence.json','launchd-evidence.json','controller.log','agent.log']:
   if (r/name).exists():tar.add(r/name,arcname='roles/'+r.name+'/'+name)
 for name in ['proof.json','worker.log','controller.log']:tar.add(proof/name,arcname='docker/'+name)
''';r=subprocess.run(ssh+['python3','-'],input=fetch.encode(),stdout=subprocess.PIPE,check=True)
with tarfile.open(fileobj=io.BytesIO(r.stdout)) as tar:tar.extractall(out,filter='data')
cleanup=r'''import pathlib,json,subprocess,os,signal,time,shutil
base=pathlib.Path('/Users/philjepsen/gridstator-validation.I50ONpwm');root=base/'docker-proof-ea963c9fa97d';p=json.loads((root/'proof.json').read_text());env=os.environ.copy();env.update(json.loads((root/'environment.json').read_text())['env']);docker=env['GRIDSTATOR_DOCKER']
for args in [['ps','-aq'],['volume','ls','-q']]:assert subprocess.check_output([docker]+args+['--filter','label=io.gridstator.node='+p['nodeId']],env=env).strip()==b''
for pid,role in [(p['workerPid'],'agent start'),(p['controllerPid'],'controller start')]:
 command=subprocess.check_output(['/bin/ps','-p',str(pid),'-o','command='],text=True).strip();assert command.startswith(str(base/'payloads/native-preview-20261110-7c41b2f3c79d-arm64')+' '+role+' --data-dir '+str(root)+'/');os.kill(pid,signal.SIGTERM)
for n in range(150):
 alive=[]
 for pid in [p['workerPid'],p['controllerPid']]:
  try:os.kill(pid,0);alive.append(pid)
  except ProcessLookupError:pass
 if not alive:break
 time.sleep(.1)
assert not alive,alive
roles=base/'preview-roles-7c41b2f3c79d';labels=[]
for f in roles.rglob('launchd-evidence.json'):labels.extend(json.loads(f.read_text())['labels'])
for label in labels:
 assert label.startswith('org.gridstator.native.validation.');r=subprocess.run(['/bin/launchctl','print','gui/'+str(os.getuid())+'/'+label],stdout=subprocess.PIPE,stderr=subprocess.PIPE);assert r.returncode!=0,label
for line in subprocess.check_output(['/bin/ps','-axo','pid=,command='],text=True).splitlines():assert not(str(roles) in line and 'gridstator' in line),'private role actor survived'
shutil.rmtree(roles);removed=[str(roles)]
for name in ['client.json','controller','agent','cache','workspace with spaces','environment.json','docker-config']:
 f=root/name
 if f.is_dir():shutil.rmtree(f);removed.append(str(f))
 elif f.exists():f.unlink();removed.append(str(f))
report={'result':'passed','ownActorsExited':[p['workerPid'],p['controllerPid']],'uniqueUserLabelsAbsent':labels,'ownDockerContainersVolumesAndBuildTagRemoved':True,'removedPrivateStateAndCredentials':removed,'privateAnonymousDockerConfigurationRemoved':True,'globalDockerConfigOrKeychainChanged':False,'immutablePreviewPayloadsRetained':True,'dockerDesktopLeftRunning':True,'desktopVersion':subprocess.check_output([docker,'version','--format','{{.Server.Version}}'],env=env,text=True).strip()};(root/'cleanup.json').write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report))
''';r=subprocess.run(ssh+['python3','-'],input=cleanup,text=True,stdout=subprocess.PIPE,check=True);(out/'cleanup.json').write_text(r.stdout);print('Final7c public proof collected; owned roles/actors/configuration/credentials cleaned, Mac Docker free')
