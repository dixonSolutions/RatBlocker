<#
.SYNOPSIS
  RatBlocker guided setup for Windows. Needs PowerShell, and nothing else.

.DESCRIPTION
  Finds the browsers on this machine, asks which to install into, and does it.
  This is the Windows half of the same specification `setup.sh` implements for
  Linux and macOS; `tests/setup-script.test.mjs` holds both to it.

  No browser is named anywhere in this file, and that is the point. Browsers
  are found by asking the machine what is installed, and identified by the
  engine they actually ship:

    Gecko     xul.dll, or application.ini beside omni.ja
    Chromium  resources.pak beside icudtl.dat or chrome.dll

  A fork released after this was written ships those files too, so it is found
  on its own terms. Everything else is read out of the installation: the name
  and version from application.ini, and the profile directory the way Gecko
  itself derives it, confirmed against the compatibility.ini Gecko leaves in
  each profile naming the installation it last ran from.

  Two rules hold throughout. Nothing found is ever executed — asking a binary
  its version is how you open half a dozen windows on someone's desktop, since
  plenty of things that embed a browser engine are not browsers. And embedding
  an engine is not being a browser: every Electron application ships Chromium's
  .pak files, and a mail client ships the same Gecko engine.

  Gecko browsers install per profile and need no privileges. A store-free
  Chromium install on Windows goes through enterprise policy, which writes to
  HKLM, so those commands are printed for an elevated prompt rather than run.

.EXAMPLE
  .\setup.ps1
  .\setup.ps1 -All -Yes
  .\setup.ps1 -DryRun
  .\setup.ps1 -Uninstall
  .\setup.ps1 -Xpi https://addons.mozilla.org/firefox/downloads/latest/ratblocker/latest.xpi
#>
[CmdletBinding()]
param(
  [switch] $Uninstall,
  [switch] $Update,
  [switch] $DryRun,
  [switch] $All,
  [Alias('Y')] [switch] $Yes,
  [switch] $List,
  [switch] $Json,
  [string] $Select,
  [string] $Xpi,
  [string] $Home_,
  [switch] $NoSystemRoots
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$GeckoId = 'ratblocker@ratblocker.github.io'
$PrefMarker = '// added by RatBlocker setup'
$AmoSlug = 'ratblocker'
$AmoLatest = "https://addons.mozilla.org/firefox/downloads/latest/$AmoSlug/latest.xpi"

$Here = Split-Path -Parent $MyInvocation.MyCommand.Path
$Dist = Join-Path (Split-Path -Parent $Here) 'dist'
$ScanHome = if ($Home_) { $Home_ } else { $env:USERPROFILE }
if (-not $ScanHome) { $ScanHome = $HOME }
$Mode = if ($Uninstall) { 'uninstall' } elseif ($Update) { 'update' } else { 'install' }

function Say { param([string] $Text = '') if (-not $Json) { Write-Host $Text } }

# ------------------------------------------------------------------ INI files

function Read-Ini {
  param([string] $Path)
  $result = @{}
  if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return $result }
  $section = ''
  foreach ($raw in (Get-Content -LiteralPath $Path -ErrorAction SilentlyContinue)) {
    $line = $raw.Trim()
    if ($line -eq '' -or $line.StartsWith(';') -or $line.StartsWith('#')) { continue }
    if ($line.StartsWith('[') -and $line.EndsWith(']')) {
      $section = $line.Substring(1, $line.Length - 2)
      if (-not $result.ContainsKey($section)) { $result[$section] = @{} }
      continue
    }
    $eq = $line.IndexOf('=')
    if ($eq -lt 0) { continue }
    if (-not $result.ContainsKey($section)) { $result[$section] = @{} }
    $result[$section][$line.Substring(0, $eq).Trim()] = $line.Substring($eq + 1).Trim()
  }
  return $result
}

function Ini-Value {
  param($Ini, [string] $Section, [string] $Key)
  if ($Ini.ContainsKey($Section) -and $Ini[$Section].ContainsKey($Key)) { return $Ini[$Section][$Key] }
  return ''
}

# -------------------------------------------------------------- engine probes

function Get-Engine {
  param([string] $Dir)
  $has = { param($n) Test-Path -LiteralPath (Join-Path $Dir $n) }
  foreach ($marker in @('xul.dll', 'libxul.so', 'XUL', 'libxul.dylib')) {
    if (& $has $marker) { return 'gecko' }
  }
  if ((& $has 'application.ini') -and ((& $has 'omni.ja') -or (& $has 'platform.ini'))) { return 'gecko' }
  if (& $has 'resources.pak') {
    foreach ($companion in @('icudtl.dat', 'chrome.dll', 'chrome_100_percent.pak')) {
      if (& $has $companion) { return 'chromium' }
    }
  }
  return ''
}

# An application that embeds an engine carries its own code beside it; a
# browser never does, because a browser's product code is the engine.
function Test-EmbeddedApplication {
  param([string] $Dir)
  foreach ($rel in @('resources\app.asar', 'resources\app', 'resources\default_app.asar')) {
    if (Test-Path -LiteralPath (Join-Path $Dir $rel)) { return $true }
  }
  return $false
}

# A Gecko engine is not necessarily a browser: the browser application code
# lives in browser\omni.ja, and a mail client ships isp\ and no browser\.
function Test-GeckoBrowser {
  param([string] $Dir)
  return (Test-Path -LiteralPath (Join-Path $Dir 'browser'))
}

# ------------------------------------------------------------- Gecko details

function Get-GeckoIni {
  param([string] $Dir)
  foreach ($rel in @('browser\application.ini', 'application.ini')) {
    $path = Join-Path $Dir $rel
    if (Test-Path -LiteralPath $path -PathType Leaf) { return (Read-Ini $path) }
  }
  return @{}
}

# Where Gecko will keep this application's profiles, derived the way Gecko
# derives it: [App] Profile wins if the build sets one, otherwise Vendor\Name.
function Get-ProfileRelative {
  param($Ini)
  $key = Ini-Value $Ini 'App' 'Profile'
  $vendor = Ini-Value $Ini 'App' 'Vendor'
  $name = Ini-Value $Ini 'App' 'Name'
  if ($key) { return ($key -replace '/', '\') }
  if ($vendor) { return (Join-Path $vendor $name) }
  return $name
}

# Does this build enforce Mozilla's signature check?
#
# There is no way to ask a binary, and the answer decides whether an unsigned
# XPI installs or is silently rejected. MOZ_REQUIRE_SIGNING is compiled in for
# Mozilla's own release and beta builds and nothing else, so the build is asked
# what it is: a shipped default that turns the check off settles it, then the
# update channel, then who built it.
function Get-SigningPolicy {
  param([string] $Dir, $Ini)
  foreach ($rel in @('browser\defaults\preferences', 'defaults\pref', 'defaults\preferences')) {
    $prefDir = Join-Path $Dir $rel
    if (-not (Test-Path -LiteralPath $prefDir)) { continue }
    foreach ($file in Get-ChildItem -LiteralPath $prefDir -Filter '*.js' -File -ErrorAction SilentlyContinue) {
      $text = Get-Content -LiteralPath $file.FullName -Raw -ErrorAction SilentlyContinue
      if ($text -match 'xpinstall\.signatures\.required["'']?\s*,\s*false') {
        return @{ Enforces = $false
                  Reason = 'the build ships a default that turns the signature check off' }
      }
    }
  }

  $channel = ''
  foreach ($rel in @('defaults\pref\channel-prefs.js', 'browser\defaults\preferences\channel-prefs.js')) {
    $path = Join-Path $Dir $rel
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { continue }
    $text = Get-Content -LiteralPath $path -Raw -ErrorAction SilentlyContinue
    if ($text -match 'app\.update\.channel["'']?\s*,\s*["'']([a-z-]+)') { $channel = $Matches[1]; break }
  }

  $vendor = Ini-Value $Ini 'App' 'Vendor'
  $source = Ini-Value $Ini 'App' 'SourceRepository'
  if ($vendor -notmatch '(?i)mozilla' -or $source -notmatch '(?i)hg\.mozilla\.org|mozilla-(release|beta|central|esr)') {
    return @{ Enforces = $false
              Reason = 'not a Mozilla build, so the signature check is almost certainly not compiled in' }
  }
  if ($channel -eq '' -or $channel -eq 'release' -or $channel -eq 'beta') {
    $named = if ($channel) { $channel } else { 'release' }
    return @{ Enforces = $true
              Reason = "a Mozilla $named build enforces signing and would reject an unsigned XPI" }
  }
  return @{ Enforces = $false
            Reason = "a Mozilla $channel build honours xpinstall.signatures.required" }
}

# ------------------------------------------------------------------ scanning

function Get-ScanRoots {
  $roots = @()
  if (-not $NoSystemRoots) {
    foreach ($var in @('ProgramFiles', 'ProgramFiles(x86)', 'ProgramW6432')) {
      $value = [Environment]::GetEnvironmentVariable($var)
      if ($value -and (Test-Path -LiteralPath $value)) { $roots += $value }
    }
  }
  foreach ($rel in @('AppData\Local\Programs', 'AppData\Local', '.local\lib', 'Applications')) {
    $path = Join-Path $ScanHome $rel
    if (Test-Path -LiteralPath $path) { $roots += $path }
  }
  return ($roots | Select-Object -Unique)
}

# Directories under a root that hold an engine, found by looking for the
# engine's own files rather than for any particular name.
function Find-Installations {
  param([string] $Root, [int] $Depth = 3)
  $markers = @('xul.dll', 'libxul.so', 'XUL', 'resources.pak')
  $found = @()
  foreach ($marker in $markers) {
    $hits = Get-ChildItem -LiteralPath $Root -Recurse -Depth $Depth -Filter $marker `
              -File -Force -ErrorAction SilentlyContinue
    foreach ($hit in $hits) { $found += $hit.DirectoryName }
  }
  return ($found | Select-Object -Unique)
}

function Get-Executable {
  param([string] $Dir, [string] $Hint)
  foreach ($candidate in @("$Hint.exe", (Split-Path -Leaf $Dir) + '.exe')) {
    if ($Hint -and (Test-Path -LiteralPath (Join-Path $Dir $candidate))) {
      return (Join-Path $Dir $candidate)
    }
  }
  $exe = Get-ChildItem -LiteralPath $Dir -Filter '*.exe' -File -ErrorAction SilentlyContinue |
         Select-Object -First 1
  if ($exe) { return $exe.FullName }
  # A Chromium build keeps the engine in a versioned directory and the
  # launcher beside it, one level up.
  $parent = Split-Path -Parent $Dir
  if ($parent) {
    $exe = Get-ChildItem -LiteralPath $parent -Filter '*.exe' -File -ErrorAction SilentlyContinue |
           Select-Object -First 1
    if ($exe) { return $exe.FullName }
  }
  return ''
}

# ----------------------------------------------------------------- discovery

$Browsers = [System.Collections.ArrayList]::new()
$Skipped = [System.Collections.ArrayList]::new()
$SeenDirs = @{}

function Add-Installation {
  param([string] $Dir)
  if ($SeenDirs.ContainsKey($Dir)) { return }
  $engine = Get-Engine $Dir
  if (-not $engine) { return }
  $SeenDirs[$Dir] = $true

  if (Test-EmbeddedApplication $Dir) {
    [void] $Skipped.Add("$Dir - embeds a browser engine but ships its own application code")
    return
  }

  if ($engine -eq 'gecko') {
    if (-not (Test-GeckoBrowser $Dir)) {
      [void] $Skipped.Add("$Dir - a Gecko application with no browser\ directory, so not a browser")
      return
    }
    $ini = Get-GeckoIni $Dir
    $name = Ini-Value $ini 'App' 'Name'
    if (-not $name) { $name = Split-Path -Leaf $Dir }
    $signing = Get-SigningPolicy $Dir $ini
    $relative = Get-ProfileRelative $ini
    $roots = @()
    $candidate = Join-Path (Join-Path $ScanHome 'AppData\Roaming') $relative
    if (Test-Path -LiteralPath $candidate) { $roots += $candidate }
    [void] $Browsers.Add([pscustomobject]@{
      Name = $name
      Engine = 'gecko'
      Version = (Ini-Value $ini 'App' 'Version')
      Packaging = if ($Dir.StartsWith($ScanHome)) { 'user' } else { 'system' }
      InstallDir = $Dir
      Binary = (Get-Executable $Dir (Ini-Value $ini 'App' 'RemotingName'))
      ProfileRoots = $roots
      Signing = if ($signing.Enforces) { 'yes' } else { 'no' }
      SigningReason = $signing.Reason
      ExternalDir = ''
    })
    return
  }

  # A Chromium build keeps its engine in a versioned directory; report the
  # product directory, which is what the user recognises.
  $productDir = $Dir
  if ((Split-Path -Leaf $Dir) -match '^[0-9]+(\.[0-9]+){2,}$') { $productDir = Split-Path -Parent $Dir }
  [void] $Browsers.Add([pscustomobject]@{
    Name = (Split-Path -Leaf (Split-Path -Parent $productDir))
    Engine = 'chromium'
    Version = if ((Split-Path -Leaf $Dir) -match '^[0-9]+(\.[0-9]+){2,}$') { Split-Path -Leaf $Dir } else { '' }
    Packaging = if ($Dir.StartsWith($ScanHome)) { 'user' } else { 'system' }
    InstallDir = $productDir
    Binary = (Get-Executable $productDir 'chrome')
    ProfileRoots = @()
    Signing = ''
    SigningReason = ''
    ExternalDir = ''
  })
}

foreach ($root in Get-ScanRoots) {
  foreach ($dir in Find-Installations $root 3) { Add-Installation $dir }
}

# Profiles that live somewhere other than where the build says they should, and
# profiles whose installation is gone. Gecko writes the installation each
# profile last ran from into its compatibility.ini, which turns "whose profile
# is this" from a guess into a fact. A Gecko install is per profile and needs no
# binary, so a profile whose browser is gone is still installable — but a mail
# profile is not, and says so by holding mail.
$claimed = @{}
foreach ($b in $Browsers) { foreach ($r in $b.ProfileRoots) { $claimed[$r] = $true } }

$appData = Join-Path $ScanHome 'AppData\Roaming'
if (Test-Path -LiteralPath $appData) {
  $inis = Get-ChildItem -LiteralPath $appData -Recurse -Depth 2 -Filter 'profiles.ini' `
            -File -Force -ErrorAction SilentlyContinue
  foreach ($ini in $inis) {
    $root = $ini.DirectoryName
    if ($claimed.ContainsKey($root)) { continue }

    $owner = ''
    $compat = Get-ChildItem -LiteralPath $root -Recurse -Depth 1 -Filter 'compatibility.ini' `
                -File -Force -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($compat) { $owner = (Ini-Value (Read-Ini $compat.FullName) 'Compatibility' 'LastPlatformDir') }
    $owner = $owner -replace '[\\/]browser[\\/]?$', ''

    $attached = $false
    if ($owner) {
      foreach ($b in $Browsers) {
        if ($b.InstallDir -eq $owner) { $b.ProfileRoots += $root; $attached = $true; break }
      }
    }
    if ($attached) { continue }

    $mail = Get-ChildItem -LiteralPath $root -Recurse -Depth 2 -Force -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -in @('ImapMail', 'Mail', 'abook.sqlite') } | Select-Object -First 1
    if ($mail) {
      [void] $Skipped.Add("$root - a profile holding mail, not a browser profile")
      continue
    }

    [void] $Browsers.Add([pscustomobject]@{
      Name = (Split-Path -Leaf $root)
      Engine = 'gecko'
      Version = ''
      Packaging = 'profile-only'
      InstallDir = $root
      Binary = ''
      ProfileRoots = @($root)
      Signing = 'unknown'
      SigningReason = "the installation this profile belongs to was not found$(if ($owner) { " (it last ran from $owner)" })"
      ExternalDir = ''
    })
  }
}

# ----------------------------------------------------------------- artefacts

function Get-Profiles {
  param([string] $Root)
  $ini = Join-Path $Root 'profiles.ini'
  if (-not (Test-Path -LiteralPath $ini -PathType Leaf)) { return @() }
  $profiles = @()
  $path = ''; $relative = $true; $inProfile = $false
  foreach ($raw in (Get-Content -LiteralPath $ini)) {
    $line = $raw.Trim()
    if ($line.StartsWith('[')) {
      if ($inProfile -and $path) {
        $profiles += (if ($relative) { Join-Path $Root $path } else { $path })
      }
      $inProfile = $line.StartsWith('[Profile'); $path = ''; $relative = $true
      continue
    }
    if (-not $inProfile) { continue }
    if ($line.StartsWith('Path=')) { $path = $line.Substring(5) }
    if ($line -eq 'IsRelative=0') { $relative = $false }
  }
  if ($inProfile -and $path) { $profiles += (if ($relative) { Join-Path $Root $path } else { $path }) }
  return ($profiles | Where-Object { Test-Path -LiteralPath $_ })
}

$XpiPath = ''
$XpiVersion = ''
if ($Xpi) {
  if ($Xpi -match '^https?://') {
    $XpiPath = Join-Path ([System.IO.Path]::GetTempPath()) 'ratblocker.xpi'
    Invoke-WebRequest -Uri $Xpi -OutFile $XpiPath -UseBasicParsing
  } else { $XpiPath = $Xpi }
} else {
  $candidate = Get-ChildItem -LiteralPath $Dist -Filter 'ratblocker-firefox-*.xpi' `
                 -File -ErrorAction SilentlyContinue | Sort-Object Name | Select-Object -Last 1
  if ($candidate) { $XpiPath = $candidate.FullName }
}
if ($XpiPath -and ($XpiPath -match 'ratblocker-firefox-(.+)\.xpi$')) { $XpiVersion = $Matches[1] }

$CrxPath = Join-Path $Dist 'ratblocker-chromium.crx'
$HasCrx = Test-Path -LiteralPath $CrxPath -PathType Leaf
$Descriptor = ''
$ChromiumId = ''
if ($HasCrx) {
  $found = Get-ChildItem -LiteralPath $Dist -Filter '*.json' -File -ErrorAction SilentlyContinue |
           Where-Object { $_.BaseName -match '^[a-p]{32}$' } | Select-Object -First 1
  if ($found) { $Descriptor = $found.FullName; $ChromiumId = $found.BaseName }
}

function Test-XpiSigned {
  if (-not $XpiPath -or -not (Test-Path -LiteralPath $XpiPath)) { return $false }
  $bytes = [System.IO.File]::ReadAllBytes($XpiPath)
  $text = [System.Text.Encoding]::ASCII.GetString($bytes)
  return $text.Contains('META-INF/mozilla.rsa')
}
$Signed = Test-XpiSigned

# --------------------------------------------------------------------- plans

function Get-Plan {
  param($Browser)
  if ($Browser.Engine -eq 'gecko') {
    if (-not $XpiPath) {
      return @{ Status = 'blocked'; Detail = 'no XPI available; build one, or pass -Xpi <path|url>' }
    }
    if ($Browser.Signing -eq 'yes' -and -not $Signed -and $Mode -ne 'uninstall') {
      return @{ Status = 'refused'; Detail = $Browser.SigningReason }
    }
    $count = 0
    foreach ($root in $Browser.ProfileRoots) { $count += (Get-Profiles $root).Count }
    if ($count -eq 0) {
      return @{ Status = 'no-profile'; Detail = 'no profile yet - start the browser once' }
    }
    return @{ Status = 'ok'; Detail = "$count profile$(if ($count -ne 1) { 's' })" }
  }

  if (-not $Descriptor) {
    return @{ Status = 'blocked'; Detail = 'no CRX packaged; run node build.mjs && node package.mjs' }
  }
  # Chrome refuses to install an external extension that is not in the Web
  # Store on Windows, so policy is the only store-free route, and the CRX has
  # to be reachable over HTTPS at RATBLOCKER_UPDATE_BASE.
  return @{ Status = 'needs-admin'
            Detail = 'off-store Chromium installs on Windows go through enterprise policy' }
}

$Plans = @()
foreach ($b in $Browsers) { $Plans += Get-Plan $b }

if ($Json) {
  $out = @{
    platform = 'win32'
    mode = $Mode
    xpi = $XpiPath
    xpiVersion = $XpiVersion
    chromiumId = $ChromiumId
    skipped = $Skipped.Count
    browsers = @()
  }
  for ($i = 0; $i -lt $Browsers.Count; $i++) {
    $out.browsers += @{
      name = $Browsers[$i].Name
      engine = $Browsers[$i].Engine
      version = $Browsers[$i].Version
      packaging = $Browsers[$i].Packaging
      installDir = $Browsers[$i].InstallDir
      binary = $Browsers[$i].Binary
      signing = $Browsers[$i].Signing
      externalDir = $Browsers[$i].ExternalDir
      status = $Plans[$i].Status
      detail = $Plans[$i].Detail
      profileRoots = @($Browsers[$i].ProfileRoots)
    }
  }
  $out | ConvertTo-Json -Depth 6
  exit 0
}

function Get-Mark {
  param([string] $Status)
  switch ($Status) {
    'ok' { '+' } 'needs-admin' { '!' } 'refused' { 'x' }
    'blocked' { 'x' } 'no-profile' { '-' } default { '?' }
  }
}

Say ''
Say "RatBlocker setup - $Mode$(if ($DryRun) { ' (dry run)' })"
Say "  packaged   $(if ($XpiPath) { "$XpiPath$(if ($Signed) { ' (signed)' } else { ' (UNSIGNED)' })" } else { 'no XPI' })"
Say '  platform   win32'
Say ''

if ($Browsers.Count -eq 0) {
  Say 'No browser was found on this machine.'
  exit 0
}

Say 'Found:'
Say ''
for ($i = 0; $i -lt $Browsers.Count; $i++) {
  $label = "$($Browsers[$i].Name) $($Browsers[$i].Version)".Trim()
  Say ("{0,3} {1} {2,-30} {3,-9} {4}" -f ($i + 1), (Get-Mark $Plans[$i].Status),
       $label.Substring(0, [Math]::Min(30, $label.Length)), $Browsers[$i].Engine, $Browsers[$i].Packaging)
  Say "        $($Browsers[$i].InstallDir)"
  Say "        $($Plans[$i].Detail)"
}

if ($List) { exit 0 }

# ----------------------------------------------------------------- selecting

$actionable = @()
for ($i = 0; $i -lt $Plans.Count; $i++) {
  if ($Plans[$i].Status -in @('ok', 'needs-admin')) { $actionable += $i }
}

function Resolve-Selection {
  param([string] $Input, [int] $Count)
  $text = $Input.Trim().ToLower()
  if ($text -eq 'q' -or $text -eq 'quit') { return $null }
  if ($text -eq 'a' -or $text -eq 'all') { return $actionable }
  $picked = @()
  foreach ($part in ($text -split '[,\s]+' | Where-Object { $_ })) {
    if ($part -match '^([0-9]+)-([0-9]+)$') {
      for ($n = [int]$Matches[1]; $n -le [int]$Matches[2]; $n++) {
        if ($n -ge 1 -and $n -le $Count) { $picked += ($n - 1) }
      }
    } elseif ($part -match '^[0-9]+$') {
      $n = [int]$part
      if ($n -ge 1 -and $n -le $Count) { $picked += ($n - 1) }
    }
  }
  return ($picked | Select-Object -Unique)
}

$chosen = @()
if ($Select) {
  $chosen = Resolve-Selection $Select $Browsers.Count
} elseif ($All -or $Yes) {
  $chosen = $actionable
} else {
  Say ''
  Say 'Select browsers: numbers (1 3), a range (1-3), "a" for all, "q" to quit.'
  $answer = Read-Host '>'
  $chosen = Resolve-Selection $answer $Browsers.Count
  if ($null -eq $chosen -or $chosen.Count -eq 0) { Say 'Nothing selected.'; exit 0 }
  if (-not $DryRun) {
    $confirm = Read-Host "Proceed with $Mode? [y/N]"
    if ($confirm -notmatch '^(y|yes)$') { Say 'Nothing done.'; exit 0 }
  }
}
if ($null -eq $chosen -or $chosen.Count -eq 0) { Say 'Nothing selected.'; exit 0 }

# ----------------------------------------------------------------- executing

function Set-UserPrefs {
  param([string] $Profile, [switch] $Remove)
  $file = Join-Path $Profile 'user.js'
  $kept = @()
  if (Test-Path -LiteralPath $file) {
    $kept = Get-Content -LiteralPath $file | Where-Object { $_ -notlike "*$PrefMarker*" }
  }
  if ($Remove) {
    if ($kept.Count -gt 0) { Set-Content -LiteralPath $file -Value $kept }
    elseif (Test-Path -LiteralPath $file) { Remove-Item -LiteralPath $file -Force }
    return
  }
  $kept += "user_pref(`"xpinstall.signatures.required`", false); $PrefMarker"
  $kept += "user_pref(`"extensions.autoDisableScopes`", 0); $PrefMarker"
  Set-Content -LiteralPath $file -Value $kept
}

function Get-InstalledVersion {
  param([string] $Profile)
  $db = Join-Path $Profile 'extensions.json'
  if (-not (Test-Path -LiteralPath $db -PathType Leaf)) { return '' }
  try {
    $json = Get-Content -LiteralPath $db -Raw | ConvertFrom-Json
    foreach ($addon in $json.addons) { if ($addon.id -eq $GeckoId) { return $addon.version } }
  } catch { return '' }
  return ''
}

function Test-Newer {
  param([string] $A, [string] $B)
  $x = $A -split '\.'; $y = $B -split '\.'
  for ($i = 0; $i -lt [Math]::Max($x.Count, $y.Count); $i++) {
    $u = if ($i -lt $x.Count) { [int]($x[$i] -replace '\D.*$', '') } else { 0 }
    $v = if ($i -lt $y.Count) { [int]($y[$i] -replace '\D.*$', '') } else { 0 }
    if ($u -gt $v) { return $true }
    if ($u -lt $v) { return $false }
  }
  return $false
}

# Replacing the file is the whole of the install and of the update: Gecko reads
# the version out of it at startup, performs the install itself, and verifies it.
function Install-Into {
  param([string] $Profile)
  $target = Join-Path (Join-Path $Profile 'extensions') "$GeckoId.xpi"
  if ($Mode -eq 'uninstall') {
    if ($DryRun) { return "would remove $target" }
    if (Test-Path -LiteralPath $target) { Remove-Item -LiteralPath $target -Force }
    Set-UserPrefs $Profile -Remove
    return "removed $target"
  }
  if ($Mode -eq 'update') {
    $current = Get-InstalledVersion $Profile
    if ($current -and -not (Test-Newer $XpiVersion $current)) {
      return "up to date ($current) in $Profile"
    }
    $line = "$(if ($current) { $current } else { 'nothing' }) -> $XpiVersion in $Profile"
    if ($DryRun) { return $line }
    Write-Xpi $Profile $target
    return $line
  }
  if ($DryRun) { return "would install $target" }
  Write-Xpi $Profile $target
  return "installed $target"
}

function Write-Xpi {
  param([string] $Profile, [string] $Target)
  $dir = Split-Path -Parent $Target
  if (-not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
  Copy-Item -LiteralPath $XpiPath -Destination $Target -Force
  if (-not $Signed) { Set-UserPrefs $Profile }
}

Say ''
Say "$(if ($DryRun) { 'Would do' } else { 'Doing' }):"
Say ''
$privileged = @()
foreach ($i in $chosen) {
  Say "$(Get-Mark $Plans[$i].Status) $($Browsers[$i].Name)"
  if ($Plans[$i].Status -eq 'ok' -and $Browsers[$i].Engine -eq 'gecko') {
    foreach ($root in $Browsers[$i].ProfileRoots) {
      foreach ($p in (Get-Profiles $root)) { Say "    $(Install-Into $p)" }
    }
    if ($Mode -ne 'uninstall') { Say '    restart the browser to pick it up' }
  } elseif ($Plans[$i].Status -eq 'needs-admin') {
    $vendor = if ($Browsers[$i].InstallDir -match '(?i)google') { 'Google\Chrome' } else { 'Chromium' }
    $reg = Join-Path $Dist "policy\windows-$(if ($vendor -like 'Google*') { 'chrome' } else { 'chromium' }).reg"
    if ($Mode -eq 'uninstall') {
      $privileged += "reg delete `"HKLM\SOFTWARE\Policies\$vendor\ExtensionSettings\$ChromiumId`" /f"
    } else {
      $privileged += "reg import `"$reg`""
    }
    Say '    needs an elevated prompt; the commands are collected below'
  } else {
    Say "    $($Plans[$i].Detail)"
  }
}

if ($privileged.Count -gt 0) {
  Say ''
  Say 'Chrome refuses off-store external installs on Windows, so this goes through'
  Say 'enterprise policy, which writes to HKLM. Run these from an elevated prompt,'
  Say 'and serve the CRX over HTTPS at RATBLOCKER_UPDATE_BASE:'
  Say ''
  foreach ($command in $privileged) { Say "  $command" }
}

for ($i = 0; $i -lt $Plans.Count; $i++) {
  if ($Plans[$i].Status -eq 'refused') {
    Say ''
    Say 'A refused browser enforces Mozilla signing. Install the signed build instead:'
    Say "  .\setup.ps1 -Xpi $AmoLatest"
    break
  }
}

if ($Skipped.Count -gt 0) {
  Say ''
  Say "note: $($Skipped.Count) installation(s) embedding a browser engine were skipped for"
  Say '      not being browsers. Run with -List to see everything that was considered.'
}
