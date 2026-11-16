# Non-destructive tests of the actual production dispatch branches. No Pester dependency.
# Read script text from stdin so this can also run remotely without staging executable code.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$source = [Console]::In.ReadToEnd()
$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseInput($source, [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw ($errors | Out-String) }
$functions = $ast.FindAll({ param($node) $node -is [System.Management.Automation.Language.FunctionDefinitionAst] }, $true)
foreach ($definition in $functions) { . ([scriptblock]::Create($definition.Extent.Text)) }
$dispatch = $ast.FindAll({ param($node) $node -is [System.Management.Automation.Language.SwitchStatementAst] }, $true) |
  Where-Object {$_.Condition.Extent.Text -like '*request.action*'} | Select-Object -First 1
function Invoke-Branch([string]$name) {
  $clause = $dispatch.Clauses | Where-Object {$_.Item1.Extent.Text -eq ("'" + $name + "'")} | Select-Object -First 1
  if (-not $clause) { throw ('Missing production branch: ' + $name) }
  $text = $clause.Item2.Extent.Text
  . ([scriptblock]::Create($text.Substring(1, $text.Length - 2)))
}
function Assert([bool]$condition, [string]$message) { if (-not $condition) { throw $message } }
$windows = 'C:\Windows'
$wsl = 'C:\Windows\System32\wsl.exe'
$request = [pscustomobject]@{action='restart';engine='desktop'}
$report = @{}
$script:programArguments = @()
function Register-Resume { }
function Invoke-Program([string]$file, [string[]]$arguments, [string]$inputText='', [int]$timeout=900) {
  $script:programArguments = $arguments
  return @{exitCode=0;stdout='';stderr=''}
}
Invoke-Branch 'restart'
$timeoutIndex = [array]::IndexOf($script:programArguments, '/t')
Assert ($script:programArguments -contains '/r') 'The confirmed restart must request a Windows restart.'
Assert ($script:programArguments -notcontains '/f') 'Restart must not force applications closed.'
Assert ($timeoutIndex -ge 0 -and $script:programArguments[$timeoutIndex + 1] -eq '0') 'A positive shutdown timeout implicitly forces applications closed; use /t 0.'

# A present WSL package with disabled Windows features needs install, rather than update alone.
$script:readyCalls = 0
function Get-WslReady { $script:readyCalls++; return $script:readyCalls -gt 1 }
function Invoke-Wsl([string[]]$arguments, [string]$inputText='', [int]$timeout=900) {
  return @{exitCode=$(if($arguments[0] -eq '--status'){1}else{0});stdout='';stderr=''}
}
function Get-AuthenticodeSignature { return @{Status='Valid';SignerCertificate=@{Subject='CN=Microsoft Windows, O=Microsoft Corporation, C=US'}} }
function Start-Process([string]$FilePath, [string[]]$ArgumentList, [string]$Verb, [switch]$Wait, [switch]$PassThru) {
  $script:programArguments = $ArgumentList
  return @{ExitCode=0}
}
$request.action = 'wsl'
Invoke-Branch 'wsl'
Assert ($script:programArguments -contains '--install') 'Disabled Windows features require WSL installation, not merely a package update.'
Assert ($script:programArguments -contains '--no-distribution') 'Elevated WSL provisioning must not create an account-bound distribution.'
Write-Output 'PowerShell dispatch tests: 2 passed; no OS changes performed.'
