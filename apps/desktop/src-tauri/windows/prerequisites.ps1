# Embedded in the companion. User input arrives as JSON on stdin, never as script text.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$OutputEncoding = [Text.UTF8Encoding]::new($false)
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$request = [Console]::In.ReadToEnd() | ConvertFrom-Json
$report = @{ wslAvailable=$false; desktopInstalled=$false; distributions=@(); compatible=$false; owned=$false; restartRequired=$false }
$windows = [Environment]::GetFolderPath('Windows')
$wsl = Join-Path $windows 'System32\wsl.exe'
$setupRoot = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'gridstator-desktop\setup'
$ownedRoot = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) ('gridstator-desktop\wsl\' + $request.ownershipId)
$report.bootId = (Get-CimInstance Win32_OperatingSystem).LastBootUpTime.ToUniversalTime().Ticks.ToString()

function Quote-Argument([string]$value) {
  if ($value.Length -gt 0 -and $value -notmatch '[\s"]') { return $value }
  return '"' + [regex]::Replace([regex]::Replace($value, '(\\*)"', '$1$1\"'), '(\\+)$', '$1$1') + '"'
}
function Decode-Output([byte[]]$bytes) {
  $utf16=$bytes.Length -ge 2 -and (($bytes[0] -eq 255 -and $bytes[1] -eq 254) -or $bytes[1] -eq 0)
  if ($utf16) { return [Text.Encoding]::Unicode.GetString($bytes).TrimStart([char]0xFEFF) }
  return [Text.Encoding]::UTF8.GetString($bytes)
}
function Invoke-Program([string]$file, [string[]]$arguments, [string]$inputText='', [int]$timeout=900) {
  $info = [Diagnostics.ProcessStartInfo]::new()
  $info.FileName = $file
  $info.Arguments = ($arguments | ForEach-Object { Quote-Argument $_ }) -join ' '
  $info.UseShellExecute = $false
  $info.CreateNoWindow = $true
  $info.RedirectStandardInput = $true
  $info.RedirectStandardOutput = $true
  $info.RedirectStandardError = $true
  $info.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
  $info.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
  $process = [Diagnostics.Process]::new()
  $process.StartInfo = $info
  if (-not $process.Start()) { throw 'A setup helper could not start.' }
  $outBytes=[IO.MemoryStream]::new()
  $errBytes=[IO.MemoryStream]::new()
  $stdout = $process.StandardOutput.BaseStream.CopyToAsync($outBytes)
  $stderr = $process.StandardError.BaseStream.CopyToAsync($errBytes)
  if ($inputText.Length -gt 0) { $process.StandardInput.Write($inputText) }
  $process.StandardInput.Close()
  if (-not $process.WaitForExit($timeout * 1000)) {
    $taskkill = Join-Path $windows 'System32\taskkill.exe'
    & $taskkill /PID $process.Id /T /F 2>&1 | Out-Null
    throw 'The setup helper timed out. Progress has been retained; retry after checking the indicated component.'
  }
  $stdout.GetAwaiter().GetResult()
  $stderr.GetAwaiter().GetResult()
  $result = @{ exitCode=$process.ExitCode; stdout=(Decode-Output $outBytes.ToArray()); stderr=(Decode-Output $errBytes.ToArray()) }
  $outBytes.Dispose();$errBytes.Dispose()
  $process.Dispose()
  return $result
}
function Invoke-Wsl([string[]]$arguments, [string]$inputText='', [int]$timeout=900) {
  return Invoke-Program $wsl $arguments $inputText $timeout
}
function Invoke-Linux([string]$user, [string]$script, [string[]]$parameters=@(), [int]$timeout=900) {
  return Invoke-Wsl (@('--distribution',$request.distribution,'--user',$user,'--exec','/bin/sh','-s','--') + $parameters) $script $timeout
}
function Require-Success($result, [string]$message) {
  if ($result.exitCode -ne 0) {
    $detail = ($result.stderr + ' ' + $result.stdout).Trim()
    if ($detail.Length -gt 1500) { $detail=$detail.Substring($detail.Length-1500) }
    throw ($message + ' ' + $detail)
  }
}
function Read-Distributions {
  $path='HKCU:\Software\Microsoft\Windows\CurrentVersion\Lxss'
  $running=@()
  if (Test-Path -LiteralPath $wsl) {
    $result=Invoke-Wsl @('--list','--running','--quiet') '' 20
    if ($result.exitCode -eq 0) { $running=@($result.stdout -split '\r?\n' | ForEach-Object {$_.Trim()} | Where-Object {$_}) }
  }
  if (Test-Path $path) {
    foreach ($key in Get-ChildItem $path) {
      $entry=Get-ItemProperty -LiteralPath $key.PSPath
      if ($entry.DistributionName) {
        @{ name=[string]$entry.DistributionName; registration=[string]$key.PSChildName; version=[uint32]$entry.Version; state=$(if($running -contains $entry.DistributionName){'running'}else{'stopped'}); basePath=[string]$entry.BasePath }
      }
    }
  }
}
function Get-Selected {
  return @(Read-Distributions | Where-Object {$_.name -ceq $request.distribution}) | Select-Object -First 1
}
function Get-WslReady {
  if (-not (Test-Path -LiteralPath $wsl)) { return $false }
  $version=Invoke-Wsl @('--version') '' 20
  $status=Invoke-Wsl @('--status') '' 20
  if ($version.exitCode -ne 0 -or $status.exitCode -ne 0) { return $false }
  $match=[regex]::Match($version.stdout,'\d+\.\d+\.\d+')
  return $match.Success -and ([version]$match.Value -ge [version]'2.4.10')
}
function Docker-Signature([string]$path) {
  $signature=Get-AuthenticodeSignature -LiteralPath $path
  if ($signature.Status -ne 'Valid' -or $null -eq $signature.SignerCertificate) { throw 'The Docker executable does not have a valid Authenticode signature.' }
  $publisher=$signature.SignerCertificate.GetNameInfo([Security.Cryptography.X509Certificates.X509NameType]::SimpleName,$false)
  if (@('Docker Inc','Docker Inc.') -cnotcontains $publisher) { throw 'The Docker executable was not signed by the expected Docker publisher.' }
}
function Desktop-Executable {
  $paths=@(
    (Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Programs\DockerDesktop\Docker Desktop.exe'),
    (Join-Path ([Environment]::GetFolderPath('ProgramFiles')) 'Docker\Docker\Docker Desktop.exe')
  )
  foreach ($path in $paths) {
    if (Test-Path -LiteralPath $path -PathType Leaf) { Docker-Signature $path; return $path }
  }
  return $null
}
function Inspect-Linux {
  $selected=Get-Selected
  if ($null -eq $selected -or $selected.version -ne 2) { return }
  $probe=@'
set -eu
. /etc/os-release
printf 'OS=%s\n' "$ID"
printf 'USER=%s\n' "$(id -un)"
printf 'SYSTEMD=%s\n' "$(cat /proc/1/comm)"
if command -v docker >/dev/null 2>&1; then
  docker info --format 'DOCKER={{.OperatingSystem}}' 2>/dev/null || true
fi
'@
  $user=$request.linuxUser
  if (-not $user) {
    $default=Invoke-Wsl @('--distribution',$request.distribution,'--exec','/usr/bin/id','-un') '' 30
    Require-Success $default 'The Linux account could not be detected.'
    $user=$default.stdout.Trim()
  }
  $result=Invoke-Linux $user $probe @() 45
  if ($result.exitCode -eq 0) {
    $report.compatible=$result.stdout -match '(?m)^OS=ubuntu\r?$'
    $report.linuxUser=$user
    if ($result.stdout -match '(?m)^DOCKER=(.+)') {
      $report.detectedEngine=$(if($Matches[1] -match 'Docker Desktop'){'desktop'}else{'engine'})
    }
  }
}
function Verify-Ownership {
  $selected=Get-Selected
  if ($null -eq $selected -or $selected.version -ne 2) { throw 'The dedicated WSL2 distribution is unavailable.' }
  if ($request.ownedRegistration -and $selected.registration -cne $request.ownedRegistration) { throw 'The distribution registration changed; refusing to modify or delete a replacement.' }
  $owner=Invoke-Linux 'root' 'cat /etc/gridstator-setup-owner' @() 20
  if ($owner.exitCode -ne 0 -or $owner.stdout.Trim() -cne $request.ownershipId) { throw 'This distribution is not owned by this Gridstator setup.' }
  return $selected
}
function Register-Resume {
  $executable=[string]$request.executable
  if (-not [IO.Path]::IsPathRooted($executable) -or $executable.Contains('"') -or -not (Test-Path -LiteralPath $executable -PathType Leaf)) { throw 'The Gridstator resume executable is invalid.' }
  $key='HKCU:\Software\Microsoft\Windows\CurrentVersion\RunOnce'
  New-Item -Path $key -Force | Out-Null
  New-ItemProperty -Path $key -Name 'GridstatorSetupResume' -Value ('"' + $executable + '" --resume-setup') -PropertyType String -Force | Out-Null
}

try {
  switch ([string]$request.action) {
    'inspect' {
      $report.wslReady=Get-WslReady
      if (Test-Path -LiteralPath $wsl) { $report.wslAvailable=(Invoke-Wsl @('--status') '' 20).exitCode -eq 0 }
      $report.desktopInstalled=$null -ne (Desktop-Executable)
      $report.distributions=@(Read-Distributions | ForEach-Object { @{name=$_.name;registration=$_.registration;version=$_.version;state=$_.state} })
      if (@($report.distributions | Where-Object {$_.version -eq 2}).Count -gt 0) { $report.wslAvailable=$true }
      $selected=Get-Selected
      if ($selected) {
        # Dedicated setup uses an explicit root executable, which bypasses Ubuntu's interactive OOBE.
        if ($request.distribution -eq 'Gridstator-Ubuntu' -and -not $request.ownedRegistration) { $request.linuxUser='root' }
        Inspect-Linux
      }
    }
    'wsl' {
      if (-not (Get-WslReady)) {
        $arguments=@('--install','--no-distribution','--no-launch')
        $version=Invoke-Wsl @('--version') '' 20
        $status=Invoke-Wsl @('--status') '' 20
        if ($version.exitCode -eq 0 -and $status.exitCode -eq 0) { $arguments=@('--update','--web-download') }
        # Only the Microsoft WSL feature/update operation is elevated. Ubuntu registration and
        # all account-bound provisioning below run as the original signed-in Windows user.
        $signature=Get-AuthenticodeSignature -LiteralPath $wsl
        if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch '(^|,\s*)O=Microsoft Corporation(,|$)') { throw 'The Windows WSL executable could not be authenticated.' }
        $process=Start-Process -FilePath $wsl -ArgumentList $arguments -Verb RunAs -Wait -PassThru
        if ($process.ExitCode -notin @(0,3010)) { throw ('Windows could not prepare WSL2 (exit ' + $process.ExitCode + '). Enable virtualization in firmware if Windows reports that requirement, then retry.') }
        $report.restartRequired=$process.ExitCode -eq 3010
      }
      $report.wslAvailable=Get-WslReady
      if (-not $report.wslAvailable) { $report.restartRequired=$true }
    }
    'distribution' {
      $selected=Get-Selected
      $dedicated=$request.distribution -eq 'Gridstator-Ubuntu'
      if ($dedicated) {
        if ($null -eq $selected) {
          New-Item -ItemType Directory -Path ([IO.Path]::GetDirectoryName($ownedRoot)) -Force | Out-Null
          $result=Invoke-Wsl @('--install','--distribution','Ubuntu-24.04','--name','Gridstator-Ubuntu','--no-launch','--web-download','--location',$ownedRoot) '' 1200
          Require-Success $result 'Ubuntu could not be installed through Microsoft WSL.'
          $selected=Get-Selected
          if ($null -eq $selected) { throw 'Ubuntu installation completed without the expected distribution registration.' }
        }
        $base=[IO.Path]::GetFullPath($selected.basePath.Replace('\\?\','')).TrimEnd('\')
        $expected=[IO.Path]::GetFullPath($ownedRoot).TrimEnd('\')
        if ($base -ine $expected) { throw 'A distribution named Gridstator-Ubuntu already exists outside this setup. Select compatible existing Ubuntu or rename the foreign distribution; it will not be overwritten.' }
        if ($request.ownedRegistration -and $selected.registration -cne $request.ownedRegistration) { throw 'The dedicated distribution registration changed.' }
        $script=@'
set -eu
if pgrep -x gridstator >/dev/null 2>&1; then echo 'The dedicated distribution already has running Gridstator processes; use its worker controls instead of reprovisioning' >&2; exit 1; fi
owner=$1
if [ -e /etc/gridstator-setup-owner ]; then
  test "$(cat /etc/gridstator-setup-owner)" = "$owner" || { echo 'Distribution ownership mismatch' >&2; exit 1; }
else
  printf '%s\n' "$owner" > /etc/gridstator-setup-owner
  chmod 600 /etc/gridstator-setup-owner
fi
. /etc/os-release
test "$ID" = ubuntu
if ! id gridstator >/dev/null 2>&1; then useradd --create-home --shell /bin/bash gridstator; fi
test "$(id -u gridstator)" -ne 0
getent group docker >/dev/null || groupadd --system docker
usermod -aG docker gridstator
if [ -e /etc/wsl.conf ]; then cp -n /etc/wsl.conf /etc/wsl.conf.before-gridstator || true; fi
cat > /etc/wsl.conf <<'EOF'
[boot]
systemd=true

[user]
default=gridstator
EOF
mkdir -p /var/lib/systemd/linger
touch /var/lib/systemd/linger/gridstator
printf 'SYSTEMD=%s\n' "$(cat /proc/1/comm)"
'@
        $result=Invoke-Linux 'root' $script @($request.ownershipId) 180
        Require-Success $result 'The dedicated Ubuntu account could not be prepared.'
        $report.owned=$true;$report.registration=$selected.registration;$report.linuxUser='gridstator'
        if ($result.stdout -notmatch '(?m)^SYSTEMD=systemd\r?$') {
          # This distribution is owned, has no worker yet, and is the only distribution restarted.
          $terminated=Invoke-Wsl @('--terminate',$request.distribution) '' 30
          Require-Success $terminated 'Ubuntu must restart to enable systemd.'
        }
      } else {
        if ($null -eq $selected -or $selected.version -ne 2) { throw 'Select an existing WSL2 distribution.' }
        $probe=Invoke-Linux 'root' '. /etc/os-release; printf "%s\n" "$ID"; cat /proc/1/comm' @() 30
        Require-Success $probe 'The selected distribution could not be inspected.'
        if ($probe.stdout -notmatch '(?m)^ubuntu\r?$') { throw 'Automatic setup can reuse Ubuntu only. Choose the dedicated Gridstator Ubuntu distribution.' }
        if ($probe.stdout -notmatch '(?m)^systemd\r?$') { throw 'The existing Ubuntu distribution must already use systemd. Enable systemd and restart that distribution yourself, then retry; Gridstator will not restart your unrelated workloads.' }
        if (-not $request.linuxUser) {
          $user=Invoke-Wsl @('--distribution',$request.distribution,'--exec','/usr/bin/id','-un') '' 30
          Require-Success $user 'The existing Linux account could not be detected.'
          $request.linuxUser=$user.stdout.Trim()
        }
        $report.linuxUser=$request.linuxUser
      }
      if ($report.linuxUser -eq 'root') { throw 'Select a non-root Linux account, or use the dedicated Gridstator distribution.' }
      $check=Invoke-Linux $report.linuxUser 'test "$(cat /proc/1/comm)" = systemd; test "$(id -u)" -ne 0' @() 30
      Require-Success $check 'Ubuntu systemd and a non-root worker account are required.'
      $session=Invoke-Linux 'root' 'set -eu; user=$1; loginctl enable-linger "$user"; uid=$(id -u "$user"); systemctl start "user@$uid.service"' @($report.linuxUser) 30
      Require-Success $session 'The Linux worker sign-in service could not be prepared.'
    }
    'runtime' {
      if ($request.engine -eq 'desktop') {
        $localEngine=Invoke-Linux 'root' 'if systemctl is-active --quiet docker.service; then printf direct-engine; fi' @() 20
        if ($localEngine.stdout.Trim() -eq 'direct-engine') { throw 'This Ubuntu distribution already runs Docker Engine. Keep that engine, or explicitly remove its conflicting packages before selecting Docker Desktop. Gridstator has retained its containers and data.' }
        $desktop=Desktop-Executable
        if (-not $desktop) {
          if (-not $request.acceptDockerTerms) { throw 'Docker Desktop installation requires acceptance of Docker terms in the review.' }
          New-Item -ItemType Directory -Path $setupRoot -Force | Out-Null
          $architecture=[Environment]::GetEnvironmentVariable('PROCESSOR_ARCHITECTURE','Machine')
          $arch=switch ($architecture) { 'AMD64' {'amd64'} 'ARM64' {'arm64'} default {throw 'Docker Desktop requires Windows x64 or ARM64.'} }
          $partial=Join-Path $setupRoot ('docker-desktop-' + $request.ownershipId + '.partial.exe')
          $installer=Join-Path $setupRoot ('docker-desktop-' + $request.ownershipId + '.exe')
          Invoke-WebRequest -UseBasicParsing -Uri ('https://desktop.docker.com/win/main/' + $arch + '/Docker%20Desktop%20Installer.exe') -OutFile $partial
          Docker-Signature $partial
          Move-Item -LiteralPath $partial -Destination $installer -Force
          # Validate again immediately before execution; never execute an unchecked cached file.
          Docker-Signature $installer
          $installed=Invoke-Program $installer @('install','--user','--quiet','--backend=wsl-2','--accept-license') '' 1200
          if ($installed.exitCode -eq 3010) { $report.restartRequired=$true;break }
          Require-Success $installed 'Docker Desktop installation failed.'
          $desktop=Desktop-Executable
          if (-not $desktop) { throw 'Docker Desktop installation did not produce the expected signed executable.' }
        }
        Start-Process -FilePath $desktop | Out-Null
        # Preserve existing Docker Desktop configuration. Integration has a guided fallback instead
        # of modifying undocumented settings or changing the user’s default WSL distribution.
        $ready=$false
        for ($attempt=0;$attempt -lt 12;$attempt++) {
          $probe=Invoke-Linux $request.linuxUser 'docker info --format "{{.OperatingSystem}}"' @() 15
          if ($probe.exitCode -eq 0 -and $probe.stdout -match 'Docker Desktop') { $ready=$true;break }
          Start-Sleep -Seconds 2
        }
        if (-not $ready) { $report.actionRequired='In Docker Desktop, open Settings > Resources > WSL integration. Enable integration for ' + $request.distribution + ', choose Apply & restart, then return here and choose Verify and continue. If your organization blocks this setting, contact its administrator or explicitly choose Docker Engine.' }
      } elseif ($request.engine -eq 'engine') {
        $existing=Invoke-Linux $request.linuxUser 'docker info --format "{{.OperatingSystem}}"' @() 20
        if ($existing.exitCode -eq 0 -and $existing.stdout -match 'Docker Desktop') { throw 'Docker Desktop integration is already active in this Ubuntu distribution. Reuse Docker Desktop, or explicitly disable that integration before installing Docker Engine.' }
        $script=@'
set -eu
user=$1
. /etc/os-release
test "$ID" = ubuntu
case "$VERSION_ID" in 22.04|24.04|26.04) ;; *) echo 'Choose a supported Ubuntu LTS distribution' >&2; exit 1;; esac
if ! command -v docker >/dev/null 2>&1 || ! command -v dockerd >/dev/null 2>&1 || ! systemctl cat docker.service >/dev/null 2>&1; then
  if dpkg-query -W -f='${Status}' podman-docker docker.io containerd runc 2>/dev/null | grep -q 'install ok installed'; then
    echo 'Existing container packages need manual review; Gridstator will not remove them' >&2; exit 1
  fi
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  apt-get install -y -qq ca-certificates curl
  install -m 0755 -d /etc/apt/keyrings
  curl --fail --silent --show-error --proto '=https' --tlsv1.2 https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc
  chmod a+r /etc/apt/keyrings/docker.asc
  printf 'Types: deb\nURIs: https://download.docker.com/linux/ubuntu\nSuites: %s\nComponents: stable\nArchitectures: %s\nSigned-By: /etc/apt/keyrings/docker.asc\n' "$VERSION_CODENAME" "$(dpkg --print-architecture)" > /etc/apt/sources.list.d/gridstator-docker.sources
  apt-get update -qq
  apt-get install -y -qq docker-ce docker-ce-cli containerd.io docker-buildx-plugin
fi
if ! docker buildx version >/dev/null 2>&1; then
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  apt-get install -y -qq ca-certificates curl
  install -m 0755 -d /etc/apt/keyrings
  curl --fail --silent --show-error --proto '=https' --tlsv1.2 https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc
  chmod a+r /etc/apt/keyrings/docker.asc
  printf 'Types: deb\nURIs: https://download.docker.com/linux/ubuntu\nSuites: %s\nComponents: stable\nArchitectures: %s\nSigned-By: /etc/apt/keyrings/docker.asc\n' "$VERSION_CODENAME" "$(dpkg --print-architecture)" > /etc/apt/sources.list.d/gridstator-docker.sources
  apt-get update -qq
  apt-get install -y -qq docker-buildx-plugin
fi
getent group docker >/dev/null || groupadd --system docker
usermod -aG docker "$user"
systemctl enable --now docker.service
loginctl enable-linger "$user"
uid=$(id -u "$user")
systemctl start "user@$uid.service"
manager=$(systemctl show "user@$uid.service" --property MainPID --value)
group=$(getent group docker | cut -d: -f3)
if [ "$manager" -gt 0 ] && ! grep '^Groups:' "/proc/$manager/status" | grep -qw "$group"; then
  echo 'Save your work and restart this Ubuntu distribution, then retry setup. Its existing user session needs the new Docker group; Gridstator has not interrupted other services.' >&2
  exit 1
fi
'@
        $result=Invoke-Linux 'root' $script @($request.linuxUser) 1200
        Require-Success $result 'Docker Engine could not be prepared without replacing existing packages.'
      } else { throw 'Choose Docker Engine or Docker Desktop.' }
    }
    {$_ -eq 'verify' -or $_ -eq 'verifyExisting'} {
      $script=@'
set -eu
test "$(id -u)" -ne 0
test "$(cat /proc/1/comm)" = systemd
docker info --format "ENGINE={{.OperatingSystem}}"
docker buildx version >/dev/null
# Check the service context as well as the fresh WSL shell: supplementary groups
# can differ when a reused Linux user manager predates Docker installation.
systemd-run --user --wait --pipe --collect --quiet /bin/sh -c 'set -e; docker info >/dev/null; docker buildx version >/dev/null'
image=alpine:3.22
docker run --rm --network none --read-only --cap-drop ALL --security-opt no-new-privileges --pids-limit 32 --memory 64m --cpus 0.25 "$image" sh -c 'test "$(uname -s)" = Linux; printf gridstator-runtime-ready'
'@
      $result=Invoke-Linux $request.linuxUser $script @() 300
      Require-Success $result 'The selected Linux account could not complete a Docker container check.'
      if ($request.engine -eq 'desktop' -and $result.stdout -notmatch '(?m)^ENGINE=.*Docker Desktop') { throw 'Docker Desktop was selected, but this distribution is connected to another engine. Review its engine selection.' }
      if ($request.engine -eq 'engine' -and $result.stdout -match '(?m)^ENGINE=.*Docker Desktop') { throw 'Docker Engine was selected, but Docker Desktop integration is active. Review its engine selection.' }
      if ($result.stdout -notmatch 'gridstator-runtime-ready') { throw 'Docker returned an unexpected container verification result.' }
      if ($request.action -eq 'verifyExisting') {
        $service=Invoke-Linux $request.linuxUser 'systemctl --user is-active --quiet gridstator-native-agent.service || systemctl --user is-active --quiet gridstator-native-controller.service' @() 20
        Require-Success $service 'The existing managed Gridstator service is not running. Start or repair it from the worker controls before verifying again.'
      }
      $report.wslAvailable=$true
    }
    'registerResume' { Register-Resume }
    'restart' {
      Register-Resume
      $shutdown=Join-Path $windows 'System32\shutdown.exe'
      $result=Invoke-Program $shutdown @('/r','/t','0','/c','Gridstator setup will resume after sign-in.') '' 20
      Require-Success $result 'Windows could not schedule the requested restart.'
    }
    'openDockerTerms' { Start-Process 'https://www.docker.com/legal/docker-subscription-service-agreement/' | Out-Null }
    'openDocker' {
      $desktop=Desktop-Executable
      if (-not $desktop) { throw 'Docker Desktop is not installed yet. Start or retry setup first.' }
      Start-Process -FilePath $desktop | Out-Null
    }
    'delete' {
      $selected=Verify-Ownership
      $stopped=Invoke-Linux 'root' @'
set -eu
uid=$(id -u gridstator)
if systemctl is-active --quiet "user@$uid.service"; then
  if runuser -u gridstator -- env XDG_RUNTIME_DIR=/run/user/$(id -u gridstator) systemctl --user is-active --quiet gridstator-native-agent.service gridstator-native-controller.service; then
    echo 'Stop Gridstator workers and controllers before deleting this distribution' >&2; exit 1
  fi
fi
if pgrep -x gridstator >/dev/null 2>&1; then echo 'Gridstator is still running' >&2; exit 1; fi
'@ @() 30
      Require-Success $stopped 'The dedicated distribution still has running Gridstator processes.'
      $result=Invoke-Wsl @('--unregister',$request.distribution) '' 120
      Require-Success $result 'The dedicated distribution could not be deleted.'
    }
    default { throw 'Unsupported Windows setup operation.' }
  }
  $report | ConvertTo-Json -Depth 8 -Compress
} catch {
  [Console]::Error.WriteLine($_.Exception.Message)
  exit 1
}
