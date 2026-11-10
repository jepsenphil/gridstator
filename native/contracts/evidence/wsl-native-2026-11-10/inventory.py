import json
import pathlib
import subprocess

remote = r'''
import hashlib,json,os,pathlib,platform,shutil,subprocess
def run(args,timeout=15):
    try:
        result=subprocess.run(args,text=True,capture_output=True,timeout=timeout)
        return {'exitCode':result.returncode,'stdout':result.stdout.strip(),'stderr':result.stderr.strip()}
    except Exception as error:return {'error':str(error)}
home=pathlib.Path.home()
names=('docker','act','git','curl','tar','gzip','systemctl','loginctl','openssl','ssh','python3','node','npm','cargo','code','code-insiders')
report={'platform':platform.platform(),'uname':run(['uname','-a']),'identity':run(['id']),'pid1':run(['ps','-p','1','-o','comm=']),'home':str(home),'xdgRuntimeDir':os.getenv('XDG_RUNTIME_DIR'),'commands':{name:shutil.which(name) for name in names},'sudoNonInteractive':run(['sudo','-n','true']),'userManager':run(['loginctl','show-user',str(os.getuid()),'-p','Linger','-p','State','-p','RuntimePath']),'systemGridstatorUnitFiles':run(['systemctl','list-unit-files','gridstator*','--no-legend','--no-pager']),'systemAgent':run(['systemctl','show','gridstator-agent.service','-p','ActiveState','-p','SubState','-p','MainPID','-p','UnitFileState','-p','FragmentPath']),'userGridstatorUnits':run(['systemctl','--user','list-units','--all','gridstator*','--no-legend','--no-pager'])}
agent_fields=dict(line.split('=',1) for line in report['systemAgent'].get('stdout','').splitlines() if '=' in line)
fragment=pathlib.Path(agent_fields.get('FragmentPath','/nonexistent-gridstator-unit'))
if fragment.is_file():report['existingSystemAgentUnitSha256']=hashlib.sha256(fragment.read_bytes()).hexdigest()
pid=agent_fields.get('MainPID','0')
if pid!='0':
    try:report['existingSystemAgentExecutable']=str(pathlib.Path('/proc')/pid/'exe');report['existingSystemAgentExecutableTarget']=os.readlink(pathlib.Path('/proc')/pid/'exe')
    except OSError as error:report['existingSystemAgentExecutableError']=str(error)
report['existingPaths']={}
for rel in ('.config/gridstator','.config/gridstator-v2','.local/share/gridstator','.local/share/gridstator/agent','.local/share/gridstator-v2','.vscode-server','.vscode-server-insiders','.config/systemd/user'):
    path=home/rel
    if path.exists():
        stat=path.stat();report['existingPaths'][str(path)]={'exists':True,'mode':oct(stat.st_mode&0o777),'uid':stat.st_uid,'type':'directory' if path.is_dir() else 'file'}
    else:report['existingPaths'][str(path)]={'exists':False}
report['dockerVersion']=run(['docker','version','--format','{{.Client.Version}} / {{.Server.Version}}'])
report['dockerInfo']=run(['docker','info','--format','OS={{.OSType}} Version={{.ServerVersion}} CPUs={{.NCPU}} Memory={{.MemTotal}} Root={{.DockerRootDir}} Name={{.Name}}'])
report['dockerContext']=run(['docker','context','show'])
report['actVersion']=run(['act','--version']);report['gitVersion']=run(['git','--version'])
ps='/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe'
command=r"$ErrorActionPreference='Stop'; $extensions=Join-Path $env:USERPROFILE '.vscode\extensions'; [pscustomobject]@{userProfile=$env:USERPROFILE; wslExtensions=@(Get-ChildItem -LiteralPath $extensions -Filter 'ms-vscode-remote.remote-wsl-*' -Directory -ErrorAction SilentlyContinue | Select-Object -ExpandProperty FullName); runningCodeCount=@(Get-Process -Name Code -ErrorAction SilentlyContinue).Count; distroNames=@(Get-ChildItem 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Lxss' -ErrorAction SilentlyContinue | ForEach-Object {(Get-ItemProperty -LiteralPath $_.PSPath).DistributionName})} | ConvertTo-Json -Compress"
report['windowsInterop']={'powershellExists':pathlib.Path(ps).is_file(),'metadata':run([ps,'-NoProfile','-NonInteractive','-Command',command],25)}
package=pathlib.Path('/mnt/c/Program Files/Microsoft VS Code/resources/app/package.json');product=package.with_name('product.json')
if package.is_file():report['windowsCodeVersion']=json.loads(package.read_text()).get('version')
if product.is_file():
    data=json.loads(product.read_text());report['windowsCodeProduct']={key:data.get(key) for key in ('nameLong','commit','date','quality')}
print(json.dumps(report,indent=2))
'''
command=['ssh','-o','BatchMode=yes','-o','IdentitiesOnly=yes','-o','ConnectTimeout=10','-o','ServerAliveInterval=5','-o','ServerAliveCountMax=2','-i','/home/pj/.ssh/id_rsa_wired8','pj@192.168.1.213','python3','-']
result=subprocess.run(command,input=remote,text=True,capture_output=True,timeout=120)
assert result.returncode==0,result.stderr
report=json.loads(result.stdout)
directory=pathlib.Path(__file__).parent
(directory/'inventory.json').write_text(json.dumps(report,indent=2)+'\n')
print(json.dumps({'inventory':str(directory/'inventory.json'),'systemAgent':report['systemAgent'],'systemAgentExecutable':report.get('existingSystemAgentExecutableTarget'),'windowsInterop':report['windowsInterop'],'windowsCodeVersion':report.get('windowsCodeVersion')}))
