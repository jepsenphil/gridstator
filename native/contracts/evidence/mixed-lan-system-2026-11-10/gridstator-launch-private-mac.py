import os,json,subprocess,time
from pathlib import Path
r=Path('/Users/philjepsen/gridstator-validation.I50ONpwm/mixed-cd6-20261110-31df')
config=json.loads((r/'agent'/'agent.json').read_text());assert config['controllerUrl'].startswith('https://192.168.1.162:') and config['name']=='mixed-cd6-31df-mac';(r/'agent'/'agent.json').chmod(0o600)
env=os.environ.copy()
for k in ['GRIDSTATOR_SUPERVISED','GRIDSTATOR_INSTALL_ROOT','GRIDSTATOR_INSTALL_VERSION','GRIDSTATOR_BUILD_ID']:env.pop(k,None)
env.update({'DOCKER_CONFIG':str(r/'docker-config'),'DOCKER_HOST':'unix:///Users/philjepsen/.docker/run/docker.sock','PATH':str(r/'bin')+':/usr/bin:/bin:/usr/sbin:/sbin','GRIDSTATOR_ACT':'/nonexistent/private-proof-act','GRIDSTATOR_AGENT_DATA':str(r/'agent'),'GRIDSTATOR_CACHE':str(r/'cache')})
log=open(r/'worker.log','ab',buffering=0)
p=subprocess.Popen([str(r/'install'/'supervisor'),'supervise','--install-root',str(r/'install'),'--','agent','start','--data-dir',str(r/'agent')],env=env,stdin=subprocess.DEVNULL,stdout=log,stderr=log,start_new_session=True)
(r/'supervisor.pid').write_text(str(p.pid)+'\n')
print(json.dumps({'supervisorPid':p.pid,'name':config['name'],'nodeId':config['nodeId'],'controllerUrl':config['controllerUrl'],'caFingerprint':config['caFingerprint'],'privateDockerConfig':True,'credentialHelperDirectoryExcludedFromPATH':True}))
