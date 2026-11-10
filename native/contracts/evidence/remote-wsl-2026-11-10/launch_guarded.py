"""Only launcher used after isolation review. Stock executable/plugin code; private metadata/args."""
import pathlib,sys,json,subprocess,base64,time
sys.path.insert(0,'/home/pj/gridstator-validation-v00jpyv0/gui-proof')
import window_guard as guard
mode=sys.argv[1] if len(sys.argv)>1 else 'probe'
assert mode in ['probe','full']
args=guard.base_arguments('probe-extension' if mode=='probe' else 'extension','tests/probe.cjs' if mode=='probe' else 'tests/smoke.cjs')
preflight=guard.preflight(args) # raises BEFORE any Code process or network resolver exists
(guard.GUI/('evidence/'+mode+'-guarded-preflight.json')).write_text(json.dumps(preflight,indent=2)+'\n')
q=lambda value:"'"+value.replace("'","''")+"'"
argline=subprocess.list2cmdline(args[1:]);script="$ErrorActionPreference='Stop';Get-ChildItem Env: | Where-Object {$_.Name -like 'VSCODE_*' -or $_.Name -in @('ELECTRON_RUN_AS_NODE','NODE_OPTIONS')} | Remove-Item;$p=Start-Process -FilePath "+q(guard.WIN_NATIVE+'\\CodeApp\\Code.exe')+" -WorkingDirectory "+q(guard.WIN_NATIVE+'\\CodeApp')+" -ArgumentList "+q(argline)+" -Wait -PassThru;exit $p.ExitCode"
command=[guard.PS,'-NoProfile','-NonInteractive','-EncodedCommand',base64.b64encode(script.encode('utf-16-le')).decode()]
log=(guard.GUI/('evidence/'+mode+'-guarded-client.log')).open('wb');process=subprocess.Popen(command,stdout=log,stderr=subprocess.STDOUT)
try:code=process.wait(timeout=180)
except subprocess.TimeoutExpired:raise RuntimeError('private actual WSL window timeout; inspect only owned namespace') from None
report={'mode':mode,'exitCode':code,'args':args,'preflight':preflight};(guard.GUI/('evidence/'+mode+'-guarded-exit.json')).write_text(json.dumps(report,indent=2)+'\n');assert code==0,'actual WSL window exit '+str(code)
result=guard.GUI/('evidence/host-probe.json' if mode=='probe' else 'evidence/full-smoke.json');assert result.exists(),'test host did not write bounded result';assert not(pathlib.Path.home()/'.vscode-server').exists();print(result.read_text())
