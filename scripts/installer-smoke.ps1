param(
    [Parameter(Mandatory)]
    [string]$CurrentSetup,
    [Parameter(Mandatory)]
    [string]$CurrentVersion,
    [Parameter(Mandatory)]
    [string]$CurrentSha256,
    [Parameter(Mandatory)]
    [string]$NewerSetup,
    [Parameter(Mandatory)]
    [string]$NewerVersion,
    [Parameter(Mandatory)]
    [string]$NewerSha256,
    [Parameter(Mandatory)]
    [string]$TranscriptPath,
    [switch]$Child,
    [switch]$ProbeTask,
    [string]$TestUser
)

$ErrorActionPreference = 'Stop'
$appId = '{AC539AC6-9BC0-4D2B-9B89-3D739FF1342C}_is1'
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$installKey = 'HKCU:\Software\Tilework'
$setupLog = $null

function Assert-Condition([bool]$Condition, [string]$Message) {
    if (-not $Condition) {
        throw $Message
    }
}

function Write-Pass([string]$Message) {
    Write-Output "PASS $Message"
}

function Get-ProductVersion {
    $key = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$appId"
    if (-not (Test-Path -LiteralPath $key)) {
        return $null
    }
    (Get-ItemProperty -LiteralPath $key).DisplayVersion
}

function Start-Installer([string]$Path, [string]$Label) {
    $arguments = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/SP-', "/LOG=$setupLog")
    $process = Start-Process -FilePath $Path -ArgumentList $arguments -Wait -PassThru
    Assert-Condition ($process.ExitCode -eq 0) "$Label exited with $($process.ExitCode). Log: $setupLog"
    Remove-Item -LiteralPath $setupLog -ErrorAction SilentlyContinue
}

function Assert-LegacyGuard([string]$Reason) {
    $arguments = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/SP-', "/LOG=$setupLog")
    $process = Start-Process -FilePath $CurrentSetup -ArgumentList $arguments -Wait -PassThru
    Assert-Condition ($process.ExitCode -ne 0) "Setup did not block $Reason."
    Assert-Condition (Select-String -LiteralPath $setupLog -SimpleMatch 'Legacy Tilework migration required.' -Quiet) "Setup stopped for a reason other than $Reason."
    Remove-Item -LiteralPath $setupLog
}

function Invoke-InstalledCommand([string]$Command) {
    $program = Join-Path $env:LOCALAPPDATA 'Programs/Tilework/tilework.exe'
    Assert-Condition (Test-Path -LiteralPath $program) 'Installed executable is missing.'
    $capture = Join-Path $env:TEMP 'tilework-command-output.txt'
    $errors = Join-Path $env:TEMP 'tilework-command-error.txt'
    try {
        $process = Start-Process -FilePath $program -ArgumentList $Command -RedirectStandardOutput $capture -RedirectStandardError $errors -WindowStyle Hidden -Wait -PassThru
        Assert-Condition ($process.ExitCode -eq 0) "Tilework $Command exited $($process.ExitCode): $(Get-Content -LiteralPath $errors -Raw)"
        ([string](Get-Content -LiteralPath $capture -Raw)).Trim()
    } finally {
        Remove-Item -LiteralPath $capture,$errors -ErrorAction SilentlyContinue
    }
}

function Assert-InstalledVersion([string]$Version, [string]$Label) {
    Assert-Condition ((Get-ProductVersion) -eq $Version) "$Label registry version is not $Version."
    $reported = Invoke-InstalledCommand '--version'
    Assert-Condition ($reported -eq "Tilework $Version") "$Label executable reported '$reported'."
}

function Assert-RunEntry([bool]$Enabled, [string]$Label) {
    $entry = Get-ItemProperty -LiteralPath $runKey -Name Tilework -ErrorAction SilentlyContinue
    if (-not $Enabled) {
        Assert-Condition ($null -eq $entry) "$Label unexpectedly enabled Tilework sign-in."
        return
    }
    $expected = '"' + (Join-Path $env:LOCALAPPDATA 'Programs/Tilework/tilework.exe') + '"'
    Assert-Condition ($entry.Tilework -eq $expected) "$Label sign-in command does not point to the installed executable."
}

function Assert-Shortcuts([bool]$Present, [string]$Label) {
    $programs = Join-Path $env:APPDATA 'Microsoft/Windows/Start Menu/Programs'
    $paths = @(
        (Join-Path $programs 'Tilework.lnk'),
        (Join-Path $programs 'Uninstall Tilework.lnk')
    )
    foreach ($path in $paths) {
        Assert-Condition ((Test-Path -LiteralPath $path) -eq $Present) "$Label shortcut state was unexpected at $path."
    }
}

function Assert-OfficialFiles([string]$Path, [string]$ExpectedHash, [string]$Label) {
    $resolved = (Resolve-Path -LiteralPath $Path).Path
    Assert-Condition ((Get-FileHash -Algorithm SHA256 -LiteralPath $resolved).Hash -eq $ExpectedHash.ToUpperInvariant()) "$Label SHA-256 does not match the supplied verified hash."
    Assert-Condition ((Get-AuthenticodeSignature -LiteralPath $resolved).Status -eq 'NotSigned') "$Label signature state changed; verify its publisher before use."
}

function Invoke-Lifecycle {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    Assert-Condition (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) 'Lifecycle child must use a standard user.'
    Assert-Condition ($identity.Name -like "*\$TestUser") 'Lifecycle child is running under the wrong test account.'
    # RunAs loads the test registry hive but may inherit the runner account's environment.
    $profile = [Environment]::GetFolderPath('UserProfile')
    Assert-Condition ((Split-Path -Leaf $profile) -eq $TestUser) 'The isolated Windows profile was not loaded.'
    $env:USERPROFILE = $profile
    # This freshly created, local-only QA account uses default profile folders.
    $env:LOCALAPPDATA = Join-Path $profile 'AppData/Local'
    $env:APPDATA = Join-Path $profile 'AppData/Roaming'
    $env:TEMP = (New-Item -ItemType Directory -Force -Path (Join-Path $env:LOCALAPPDATA 'Temp')).FullName
    $env:TMP = $env:TEMP
    $script:setupLog = Join-Path $env:TEMP 'tilework-installer-qa.log'

    Assert-OfficialFiles $CurrentSetup $CurrentSha256 'Current Setup'
    Assert-OfficialFiles $NewerSetup $NewerSha256 'Newer Setup'

    $programDir = Join-Path $env:LOCALAPPDATA 'Programs/Tilework'
    $legacyDir = Join-Path $env:LOCALAPPDATA 'Tilework'
    Assert-Condition (-not (Test-Path -LiteralPath $programDir)) 'Fresh test profile already contains an installed Tilework program.'
    Assert-Condition (-not (Test-Path -LiteralPath "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$appId")) 'Fresh test profile already has a Tilework uninstall registration.'
    Assert-RunEntry $false 'Fresh profile'

    $legacyExe = Join-Path $legacyDir 'tilework.exe'
    if ($ProbeTask) {
        Assert-Condition (-not (Test-Path -LiteralPath $legacyExe)) 'The task guard must be tested without a legacy executable.'
        Assert-LegacyGuard 'the disabled elevated-shaped task fixture'
        Assert-Condition (-not (Test-Path -LiteralPath $programDir)) 'The task guard wrote installation files.'
        Write-Pass 'legacy task shape alone blocks setup before installation'
        return
    }

    New-Item -ItemType Directory -Force -Path $legacyDir | Out-Null
    $legacyMarker = [Text.Encoding]::ASCII.GetBytes('non-executable Tilework migration guard fixture')
    [IO.File]::WriteAllBytes($legacyExe, $legacyMarker)
    try {
        Assert-LegacyGuard 'the legacy executable fixture'
        Assert-Condition (Test-Path -LiteralPath $legacyExe) 'The legacy path guard modified its marker file.'
        Assert-Condition (-not (Test-Path -LiteralPath $programDir)) 'The legacy path guard installed files before blocking.'
        Write-Pass 'legacy-path guard blocks setup and preserves its inert marker'
    } finally {
        if (Test-Path -LiteralPath $legacyExe) {
            Remove-Item -LiteralPath $legacyExe
        }
        if ((Test-Path -LiteralPath $legacyDir) -and -not (Get-ChildItem -LiteralPath $legacyDir -Force | Select-Object -First 1)) {
            Remove-Item -LiteralPath $legacyDir
        }
    }

    Start-Installer $CurrentSetup 'Fresh install'
    Assert-InstalledVersion $CurrentVersion 'Fresh install'
    Assert-RunEntry $false 'Fresh install'
    Assert-Shortcuts $true 'Fresh install'
    Invoke-InstalledCommand '--autostart-on' | Out-Null
    Assert-RunEntry $true 'Autostart enable'
    Assert-Condition ((Invoke-InstalledCommand '--autostart-status') -eq 'Tilework autostart entry: registered') 'Installed app autostart status did not report the entry.'
    Write-Pass 'fresh per-user install, executable launch, shortcuts, and opt-in autostart'

    $config = Join-Path $legacyDir 'config.ini'
    New-Item -ItemType Directory -Force -Path $legacyDir | Out-Null
    @(
        '# Installer lifecycle fixture'
        'focus_follows_mouse=false'
        'focus_delay_ms=725'
    ) | Set-Content -LiteralPath $config -Encoding ascii
    $configHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $config).Hash

    Start-Installer $NewerSetup 'Newer upgrade'
    Assert-InstalledVersion $NewerVersion 'Newer upgrade'
    Assert-RunEntry $true 'Newer upgrade'
    Assert-Condition ((Get-FileHash -Algorithm SHA256 -LiteralPath $config).Hash -eq $configHash) 'Newer upgrade changed the user config.'
    Write-Pass 'upgrade updates registered version and preserves config and autostart'

    $truncated = Join-Path $env:TEMP 'Tilework-Setup-truncated.exe'
    $bytes = [IO.File]::ReadAllBytes($NewerSetup)
    [IO.File]::WriteAllBytes($truncated, $bytes[0..([Math]::Min(4095, $bytes.Length - 1))])
    try {
        $failedUpgrade = $false
        try {
            $failedProcess = Start-Process -FilePath $truncated -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/SP-', "/LOG=$setupLog") -Wait -PassThru
            $failedUpgrade = $failedProcess.ExitCode -ne 0
        } catch {
            # Windows refuses to start a truncated executable; that is the expected rejection.
            $failedUpgrade = $true
        }
        Assert-Condition $failedUpgrade 'Truncated setup unexpectedly succeeded.'
        Assert-InstalledVersion $NewerVersion 'Interrupted upgrade recovery'
        Assert-RunEntry $true 'Interrupted upgrade recovery'
        Assert-Condition ((Get-FileHash -Algorithm SHA256 -LiteralPath $config).Hash -eq $configHash) 'Truncated upgrade changed the user config.'
        Remove-Item -LiteralPath $setupLog -ErrorAction SilentlyContinue
        Write-Pass 'truncated installer fails without replacing the working version or user data'
    } finally {
        Remove-Item -LiteralPath $truncated,$setupLog -ErrorAction SilentlyContinue
    }

    Start-Installer $CurrentSetup 'Downgrade to previous version'
    Assert-InstalledVersion $CurrentVersion 'Downgrade'
    Assert-RunEntry $true 'Downgrade'
    Assert-Condition ((Get-FileHash -Algorithm SHA256 -LiteralPath $config).Hash -eq $configHash) 'Downgrade changed the user config.'
    Write-Pass "verified earlier installer returns the app to $CurrentVersion and preserves user data"

    Invoke-InstalledCommand '--autostart-off' | Out-Null
    Assert-RunEntry $false 'Autostart disable'
    Invoke-InstalledCommand '--autostart-on' | Out-Null
    Assert-RunEntry $true 'Autostart re-enable'
    Write-Pass 'autostart can be disabled and re-enabled'

    $uninstaller = Join-Path $programDir 'unins000.exe'
    Assert-Condition (Test-Path -LiteralPath $uninstaller) 'Registered uninstaller executable is missing.'
    $remove = Start-Process -FilePath $uninstaller -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=$setupLog") -Wait -PassThru
    Assert-Condition ($remove.ExitCode -eq 0) "Uninstaller exited with $($remove.ExitCode). Log: $setupLog"
    Assert-Condition (-not (Test-Path -LiteralPath $programDir)) 'Uninstall left the program directory.'
    Assert-Condition (-not (Test-Path -LiteralPath "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$appId")) 'Uninstall left its Apps registration.'
    Assert-Condition (-not (Test-Path -LiteralPath $installKey)) 'Uninstall left its Tilework install-path registration.'
    Assert-RunEntry $false 'Uninstall'
    Assert-Shortcuts $false 'Uninstall'
    Assert-Condition (Test-Path -LiteralPath $config) 'Uninstall removed the settings file despite preserving user data by default.'
    Assert-Condition ((Get-FileHash -Algorithm SHA256 -LiteralPath $config).Hash -eq $configHash) 'Uninstall changed the preserved user config.'
    Remove-Item -LiteralPath $setupLog -ErrorAction SilentlyContinue
    Write-Pass 'uninstall removes program, shortcuts, Apps registration, and autostart while preserving settings'
}

Assert-Condition ($Child) 'This script must be launched by the disposable-runner account wrapper with -Child.'
Assert-Condition ($TestUser -match '^TileworkQA_[A-F0-9]{8}$') 'Test user name is invalid.'
Assert-Condition ([version]$CurrentVersion -lt [version]$NewerVersion) 'The upgrade fixture must use a newer version.'
try {
    Start-Transcript -LiteralPath $TranscriptPath -Force | Out-Null
    Invoke-Lifecycle
} catch {
    if ($setupLog -and (Test-Path -LiteralPath $setupLog)) {
        Get-Content -LiteralPath $setupLog -Tail 30 | Write-Host
    }
    Write-Host $_
    throw
} finally {
    Stop-Transcript -ErrorAction SilentlyContinue | Out-Null
}
