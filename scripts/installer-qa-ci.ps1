param([Parameter(Mandatory)][string]$Compiler)

$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'Run this account-creating test only on a disposable GitHub-hosted VM.'
}

$root = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$version = (cargo metadata --locked --no-deps --format-version 1 | ConvertFrom-Json).packages[0].version
if ($LASTEXITCODE -ne 0 -or $version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+$') {
    throw 'Failed to read the candidate version.'
}
$parts = [version]$version
$next = '{0}.{1}.{2}' -f $parts.Major, $parts.Minor, ($parts.Build + 1)
$fixture = Join-Path $env:RUNNER_TEMP 'tilework-upgrade-fixture'
git worktree add --detach $fixture $env:GITHUB_SHA
if ($LASTEXITCODE -ne 0) { throw 'Could not check out the upgrade fixture from the candidate SHA.' }

$manifest = Join-Path $fixture 'Cargo.toml'
$manifestText = (Get-Content -Raw -LiteralPath $manifest).Replace(('version = "{0}"' -f $version), ('version = "{0}"' -f $next))
Set-Content -LiteralPath $manifest -Value $manifestText -NoNewline
$lock = Join-Path $fixture 'Cargo.lock'
$pattern = '(?m)(^name = "tilework"\r?\nversion = ")' + [regex]::Escape($version) + '(")'
$lockText = [regex]::Replace((Get-Content -Raw -LiteralPath $lock), $pattern, '${1}' + $next + '${2}')
Set-Content -LiteralPath $lock -Value $lockText -NoNewline
$appManifest = Join-Path $fixture 'app.manifest'
$appManifestText = (Get-Content -Raw -LiteralPath $appManifest).Replace(('version="{0}.0"' -f $version), ('version="{0}.0"' -f $next))
Set-Content -LiteralPath $appManifest -Value $appManifestText -NoNewline

cargo build --locked --release --manifest-path $manifest
if ($LASTEXITCODE -ne 0) { throw 'The upgrade fixture did not build.' }
$fixtureExe = Join-Path $fixture 'target/release/tilework.exe'
& $Compiler "/DMyAppVersion=$next" "/DMyAppExe=$fixtureExe" (Join-Path $root 'packaging/Tilework.iss')
if ($LASTEXITCODE -ne 0) { throw 'The upgrade fixture installer did not compile.' }

$candidate = Join-Path $root 'release-files/Tilework-Setup-x64.exe'
$upgrade = Join-Path $root "packaging/output/Tilework-Setup-$next-x64.exe"
$publishedHash = ((Get-Content -LiteralPath (Join-Path $root 'release-files/SHA256SUMS.txt')) |
    Where-Object { $_ -match '  Tilework-Setup-x64\.exe$' }).Split(' ')[0]
$fixtureHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $upgrade).Hash

$qa = Join-Path $env:PUBLIC 'TileworkInstallQA'
$logs = Join-Path $qa 'logs'
New-Item -ItemType Directory -Path $qa | Out-Null
New-Item -ItemType Directory -Path $logs | Out-Null
Copy-Item -LiteralPath $candidate -Destination $qa
Copy-Item -LiteralPath $upgrade -Destination $qa
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'installer-smoke.ps1') -Destination $qa
$name = 'TileworkQA_' + [guid]::NewGuid().ToString('N').Substring(0, 8).ToUpperInvariant()
$password = ConvertTo-SecureString ('Qa!1' + [guid]::NewGuid().ToString('N')) -AsPlainText -Force
$guest = New-LocalUser -Name $name -Password $password
if (-not (Get-LocalGroupMember -SID 'S-1-5-32-545' | Where-Object SID -eq $guest.SID)) {
    Add-LocalGroupMember -SID 'S-1-5-32-545' -Member $guest
}
& icacls.exe $logs /grant "*$($guest.SID.Value):(OI)(CI)(M)" | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Failed to make installer QA logs writable for the guest.' }
$credential = [pscredential]::new(".\$name", $password)

$transcript = Join-Path $logs 'lifecycle.log'
$arguments = @('-NoProfile', '-NonInteractive', '-File', ('"' + (Join-Path $qa 'installer-smoke.ps1') + '"'),
    '-Child', '-TestUser', $name, '-TranscriptPath', ('"' + $transcript + '"'),
    '-CurrentSetup', ('"' + (Join-Path $qa (Split-Path -Leaf $candidate)) + '"'),
    '-CurrentVersion', $version, '-CurrentSha256', $publishedHash,
    '-NewerSetup', ('"' + (Join-Path $qa (Split-Path -Leaf $upgrade)) + '"'),
    '-NewerVersion', $next, '-NewerSha256', $fixtureHash)
$start = @{
    FilePath = Join-Path $PSHOME 'pwsh.exe'
    ArgumentList = $arguments
    WorkingDirectory = $qa
    Credential = $credential
    LoadUserProfile = $true
    PassThru = $true
}
$process = Start-Process @start
try {
    if (-not $process.WaitForExit(180000)) {
        $process.Kill()
        throw 'The standard-user installer test timed out.'
    }
    if ($process.ExitCode -ne 0) { throw "Standard-user test failed ($($process.ExitCode))." }
} finally {
    if (Test-Path -LiteralPath $transcript) { Get-Content -LiteralPath $transcript }
}
