import pathlib,sys,subprocess,base64,time,json
sys.path.insert(0,'/home/pj/gridstator-validation-v00jpyv0/gui-proof');import window_guard as guard
win=guard.WIN;ready=win/'pipe-guard-ready.json';stop=win/'pipe-guard-stop';name='vscode-wsl-gridstator-validation-v00jpyv0-guard-sock'
for p in [ready,stop]:
 if p.exists():p.unlink()
q=lambda value:"'"+value.replace("'","''")+"'"
s="$ErrorActionPreference='Stop';$p=[System.IO.Pipes.NamedPipeServerStream]::new("+q(name)+");try{[pscustomobject]@{windowsPid=$PID;name="+q(name)+"}|ConvertTo-Json -Compress|Set-Content -LiteralPath "+q(guard.WIN_NATIVE+'\\pipe-guard-ready.json')+";$deadline=(Get-Date).AddSeconds(15);while((-not(Test-Path -LiteralPath "+q(guard.WIN_NATIVE+'\\pipe-guard-stop')+"))-and((Get-Date)-lt$deadline)){Start-Sleep -Milliseconds 50}}finally{$p.Dispose()}"
p=subprocess.Popen([guard.PS,'-NoProfile','-NonInteractive','-EncodedCommand',base64.b64encode(s.encode('utf-16-le')).decode()],stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
try:
 for n in range(100):
  if ready.exists():break
  time.sleep(.05)
 else:raise RuntimeError('owned pipe fixture never became ready')
 fixture=json.loads(ready.read_text(encoding='utf-8-sig'))
 try:guard.preflight(guard.base_arguments())
 except AssertionError as error:assert 'preexisting WSL resolver pipe' in str(error);result={'result':'passed','ownedFixture':fixture,'rejectedBeforeCodeLaunch':str(error)}
 else:raise RuntimeError('guard accepted an actual preexisting resolver-looking pipe')
finally:stop.touch();p.wait(timeout=5)
assert p.returncode==0,p.stderr.read().decode();result['cleanPostFixturePreflight']=guard.preflight(guard.base_arguments());(guard.GUI/'evidence/pipe-guard-regression.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result,indent=2))
