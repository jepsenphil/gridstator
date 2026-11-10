"""Actual final release bytes; only new owned temporary directories change permissions."""
import pathlib,subprocess,tempfile,json,os,stat,sys,shutil
binary=pathlib.Path(sys.argv[1]);expected=sys.argv[2];evidence=pathlib.Path(sys.argv[3]);base=pathlib.Path('/Users/philjepsen/gridstator-validation.I50ONpwm');root=pathlib.Path(tempfile.mkdtemp(prefix='parent-mode-final-',dir=base));root.chmod(0o700)
def mode(path):return stat.S_IMODE(path.stat().st_mode)
env={key:value for key,value in os.environ.items() if not key.startswith('GRIDSTATOR_')};env.update(HOME=str(root),XDG_CONFIG_HOME=str(root/'xdg-config'),XDG_DATA_HOME=str(root/'xdg-data'),XDG_CACHE_HOME=str(root/'xdg-cache'));info=json.loads(subprocess.check_output([str(binary),'build-info','--json'],env=env));assert info['buildId']==expected;cases=[]
try:
 for label,relative,nested in [('absolute',False,False),('relative-leaf',True,False),('missing-nested-parents',False,True)]:
  parent=root/(label+' project');parent.mkdir(mode=0o775);parent.chmod(0o775);data=root/(label+' controller data');config=pathlib.Path('relative-client.json') if relative else parent/('new/nested/client.json' if nested else 'client.json');runenv={**env,'GRIDSTATOR_CONFIG':str(config),'GRIDSTATOR_CONTROLLER_DATA':str(data)};before=mode(parent);result=subprocess.run([str(binary),'controller','init','--host','127.0.0.1','--port','0'],cwd=parent,env=runenv,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=30);assert result.returncode==0,result.stderr.decode();file=parent/config if relative else config;assert mode(parent)==before==0o775;assert mode(file)==0o600;assert mode(data)==0o700
  extra={}
  if nested:
   assert mode(parent/'new')==0o700 and mode(parent/'new/nested')==0o700;extra={'newParent':mode(parent/'new'),'nestedParent':mode(parent/'new/nested')}
  cases.append({'case':label,'parentModeBefore':oct(before),'parentModeAfter':oct(mode(parent)),'configFileMode':oct(mode(file)),'controllerDataMode':oct(mode(data)),'newParentModes':{key:oct(value) for key,value in extra.items()},'result':'passed'})
finally:shutil.rmtree(root)
report={'result':'passed','scope':'actual final release executable custom absolute/relative config paths preserve existing0775 parents; missing nested config parents0700, config0600 and dedicated controllerdata0700; only owned directories modified','binary':info,'cases':cases,'privateFixtureRemoved':not root.exists()};evidence.write_text(json.dumps(report,indent=2)+'\n');print('Actual final-byte config parent modes passed',info['target'])
