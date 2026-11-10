"""Fail closed before every private Windows Code / Remote WSL launch. No launch on import."""
import hashlib,json,pathlib,subprocess,os,ssl,urllib.request,urllib.parse,zipfile
REMOTE=pathlib.Path('/home/pj/gridstator-validation-v00jpyv0')
GUI=REMOTE/'gui-proof'
WIN=pathlib.Path('/mnt/c/Users/User/AppData/Local/Temp/gridstator-wsl-v00jpyv0')
WIN_NATIVE=str(pathlib.PureWindowsPath('C:/Users/User/AppData/Local/Temp/gridstator-wsl-v00jpyv0'))
APP=WIN/'CodeApp'
PS='/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe'
COMMIT='a717861c07586a53d83b80d742cd3f59b20a6273'
PRIVATE_OPTIONS={
 'user-data-dir':'user-data','extensions-dir':'extensions','shared-data-dir':'shared-data',
 'agents-user-data-dir':'agents-user-data','agents-extensions-dir':'agents-extensions',
 'agent-plugins-dir':'agent-plugins','extensions-download-dir':'extensions-download',
 'crash-reporter-directory':'crash-reports',
}
def uri(path):return 'vscode-remote://wsl+Ubuntu'+str(path).replace(' ','%20')
def options(args):
 result={};index=1
 while index<len(args):
  value=args[index];assert value.startswith('--'),('unexpected positional argument',value)
  if '=' in value:key,value=value[2:].split('=',1)
  else:key,value=value[2:],None
  if value is None and index+1<len(args) and not args[index+1].startswith('--'):index+=1;value=args[index]
  assert key not in result,('duplicate argument',key);result[key]=value;index+=1
 return result
def assert_arguments(args):
 assert args[0]==str(APP/'Code.exe')
 opts=options(args)
 for option,child in PRIVATE_OPTIONS.items():assert opts.get(option)==WIN_NATIVE+'\\'+child,('missing/nonprivate option',option)
 assert 'use-inmemory-secretstorage' in opts
 assert 'new-window' in opts and opts.get('sync')=='off'
 assert opts.get('remote')=='wsl+Ubuntu'
 assert opts.get('folder-uri')==uri(GUI/'workspace with spaces')
 for key in ['extensionDevelopmentPath','extensionTestsPath']:
  assert opts.get(key,'').startswith(uri(GUI)+'/'),('nonprivate development/test URI',key)
 assert opts.get('extensionDevelopmentKind')=='workspace'
 return opts
def base_arguments(extension='probe-extension',test='tests/probe.cjs'):
 args=[str(APP/'Code.exe')]+['--'+option+'='+WIN_NATIVE+'\\'+child for option,child in PRIVATE_OPTIONS.items()]
 return args+['--use-inmemory-secretstorage','--new-window','--sync=off','--disable-updates','--skip-welcome','--skip-release-notes','--disable-workspace-trust','--disable-gpu','--remote=wsl+Ubuntu','--folder-uri='+uri(GUI/'workspace with spaces'),'--extensionDevelopmentKind=workspace','--extensionDevelopmentPath='+uri(GUI/extension),'--extensionTestsPath='+uri(GUI/test)]
def powershell(script):
 r=subprocess.run([PS,'-NoProfile','-NonInteractive','-Command',"$ErrorActionPreference='Stop'; "+script],text=True,capture_output=True,timeout=30)
 assert r.returncode==0,r.stderr
 return json.loads(r.stdout)
def verify_native_backend():
 binary=GUI/'gridstator';expected_sha='eecbbc71e3c5a2234302fd637d5d668fc641935b32e878124f1b628a59473380'
 assert hashlib.sha256(binary.read_bytes()).hexdigest()==expected_sha
 assert hashlib.sha256((GUI/'gridstator-native.vsix').read_bytes()).hexdigest()=='d0cafff141d2ca5e318a0bc0633150f2ce896e356a399bea92b44dec7ba35eb6'
 with zipfile.ZipFile(GUI/'gridstator-native.vsix') as package:
  for name in package.namelist():
   if name.startswith('extension/') and not name.endswith('/'):assert (GUI/name).read_bytes()==package.read(name)
 env={key:value for key,value in os.environ.items() if not key.startswith('GRIDSTATOR_') and key not in ['NODE_OPTIONS','ELECTRON_RUN_AS_NODE']}
 env.update({'GRIDSTATOR_CONFIG':str(GUI/'client.json'),'GRIDSTATOR_CONTROLLER_DATA':str(GUI/'local-admin-hints'),'GRIDSTATOR_AGENT_DATA':str(GUI/'agent-unused'),'GRIDSTATOR_CACHE':str(GUI/'cache')})
 info=json.loads(subprocess.check_output([str(binary),'build-info','--json'],env=env,timeout=20))
 assert info['buildId']=='native-preview-20261110-cd6b769ebe6e' and info['target']=='linux/amd64' and info['sha256']==expected_sha
 fixture=json.loads((GUI/'fixture.json').read_text());cfg=json.loads((GUI/'client.json').read_text());controller=json.loads((GUI/'backend-controller/controller.json').read_text());runtime=json.loads((GUI/'backend-controller/runtime.json').read_text())
 assert fixture['root']==str(GUI) and fixture['config']==str(GUI/'client.json') and fixture['cli']==str(binary)
 assert fixture['clusterId']==controller['clusterId']
 url=urllib.parse.urlsplit(cfg['url']);assert cfg['url']==fixture['url'] and url.scheme=='https' and url.hostname=='127.0.0.1' and url.port not in [None,7300] and not url.username and not url.password and not url.path and not url.query and not url.fragment
 assert url.port==controller['port']==runtime['port']
 assert runtime['pid']==fixture['controllerPid'];os.kill(fixture['controllerPid'],0)
 assert pathlib.Path('/proc/'+str(fixture['controllerPid'])+'/exe').resolve()==binary
 assert cfg['caPem'].count('BEGIN CERTIFICATE')==1
 digest='sha256:'+hashlib.sha256(ssl.PEM_cert_to_DER_cert(cfg['caPem'])).hexdigest();assert digest==cfg['caFingerprint']==fixture['caFingerprint']
 request=urllib.request.Request(cfg['url']+'/v1/cluster',headers={'Authorization':'Bearer '+cfg['token']})
 with urllib.request.urlopen(request,context=ssl.create_default_context(cadata=cfg['caPem']),timeout=10) as response:cluster=json.load(response)
 assert cluster['clusterId']==fixture['clusterId'] and cluster['build']['id']==info['buildId']
 return {'build':info,'clusterId':cluster['clusterId'],'url':cfg['url'],'privateConfig':fixture['config']}
def preflight(args):
 opts=assert_arguments(args)
 native=verify_native_backend() if opts['extensionDevelopmentPath']==uri(GUI/'extension') else None
 prep=json.loads((REMOTE/'private-code-preparation.json').read_text())
 exe_hash=hashlib.sha256((APP/'Code.exe').read_bytes()).hexdigest();assert exe_hash==prep['originalCodeExeSha256']
 original=json.loads(pathlib.Path('/mnt/c/Program Files/Microsoft VS Code/a717861c07/resources/app/product.json').read_text());private=json.loads((APP/'a717861c07/resources/app/product.json').read_text())
 assert {key for key in original.keys()|private.keys() if original.get(key)!=private.get(key)}=={'serverDataFolderName'}
 assert private['serverDataFolderName']=='gridstator-validation-v00jpyv0/server'
 assert not(pathlib.Path.home()/'.vscode-server').exists()
 server=REMOTE/'server/bin'/COMMIT
 product=json.loads((server/'product.json').read_text());baseline=json.loads((REMOTE/'server-product-before.json').read_text())
 assert {key for key in product.keys()|baseline.keys() if product.get(key)!=baseline.get(key)}=={'serverDataFolderName','sharedDataFolderName'}
 assert product['serverDataFolderName']=='gridstator-validation-v00jpyv0/server'
 assert product['sharedDataFolderName']=='gridstator-validation-v00jpyv0/server/shared-data'
 setup=(REMOTE/'server/server-env-setup').read_text()
 for key,value in {'VSCODE_AGENT_FOLDER':str(REMOTE/'server'),'VSCODE_PORTABLE':str(REMOTE/'server/portable'),'GRIDSTATOR_CONFIG':str(GUI/'client.json'),'GRIDSTATOR_CONTROLLER_DATA':str(GUI/'local-admin-hints'),'GRIDSTATOR_AGENT_DATA':str(GUI/'agent-unused'),'GRIDSTATOR_CACHE':str(GUI/'cache')}.items():assert "export "+key+"='"+value+"'" in setup
 machine=json.loads((REMOTE/'server/portable/user-data/Machine/settings.json').read_text())
 local=json.loads((WIN/'user-data/User/settings.json').read_text())
 assert local['gridstator.configPath']==str(GUI/'client.json') and local['gridstator.nativeCli.path']==str(GUI/'gridstator')
 assert machine['gridstator.configPath']==str(GUI/'client.json') and machine['gridstator.nativeCli.path']==str(GUI/'gridstator')
 ext=WIN/'extensions/ms-vscode-remote.remote-wsl-1.104.7'
 for name,digest in prep['remoteWslFiles'].items():assert hashlib.sha256((ext/name).read_bytes()).hexdigest()==digest
 observed=powershell(r"[pscustomobject]@{user=[Environment]::UserName;pipes=@([System.IO.Directory]::GetFiles('\\.\pipe\')|Where-Object {$_ -like '*vscode-wsl-*'});privateCode=@(Get-Process Code -ErrorAction SilentlyContinue|Where-Object {$_.Path -like '*gridstator-wsl-v00jpyv0*'}|Select-Object Id,Path);stockCodeIds=@(Get-Process Code -ErrorAction SilentlyContinue|Where-Object {$_.Path -eq 'C:\Program Files\Microsoft VS Code\Code.exe'}|Select-Object -ExpandProperty Id)}|ConvertTo-Json -Compress")
 assert not observed['pipes'],'refuse any preexisting WSL resolver pipe'
 assert not observed['privateCode'],'refuse preexisting private Code process'
 observed['effectivePipe']=r'\\.\pipe\vscode-wsl-'+hashlib.sha256(''.join([COMMIT,private.get('quality') or 'insider','ubuntu',observed['user'],'1.104.7','off']).encode()).hexdigest()+'-sock'
 if native is not None:observed['nativeBackend']=native
 return observed
if __name__=='__main__':
 args=base_arguments();assert_arguments(args);negatives=[]
 for flag in ['shared-data-dir','user-data-dir','extensions-dir','crash-reporter-directory','use-inmemory-secretstorage']:
  bad=[arg for arg in args if not arg.startswith('--'+flag)]
  try:assert_arguments(bad)
  except AssertionError:negatives.append(flag)
  else:raise RuntimeError('isolation guard accepted missing '+flag)
 bad=[arg if not arg.startswith('--shared-data-dir=') else '--shared-data-dir=C:\\Users\\User\\.vscode-shared' for arg in args]
 try:assert_arguments(bad)
 except AssertionError:negatives.append('preexisting-looking-shared-data')
 else:raise RuntimeError('guard accepted existing shared store')
 print(json.dumps({'negativeGuardsRejectedBeforeLaunch':negatives,'argv':args,'livePreflight':preflight(args)},indent=2))
