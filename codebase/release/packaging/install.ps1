# Genie CLI-only installer for Windows (spec/09 §1, D40).
#
#   irm https://<feeds>/install.ps1 | iex
#   & ([scriptblock]::Create((irm https://<feeds>/install.ps1))) -Channel beta
#
# Checks the archive's SHA-256 against the update manifest and genie.exe's Authenticode
# signature (valid chain + pinned publisher) before installing. Windows PowerShell has no
# built-in Ed25519, so the feed's Ed25519 signature is checked by `genie update` after
# install, not here (README Known gaps).
param(
  [ValidateSet('stable', 'beta', 'dev')] [string] $Channel = 'stable',
  [string] $Prefix = (Join-Path $env:LOCALAPPDATA 'Programs\Genie')
)
$ErrorActionPreference = 'Stop'
$FeedsUrl = if ($env:GENIE_FEEDS_URL) { $env:GENIE_FEEDS_URL } else { 'https://feeds.genie.invalid' }
# Authenticode publisher (certificate subject CN) of release builds. Filled in at release.
$Publisher = '@AUTHENTICODE_PUBLISHER@'
if ($Publisher.StartsWith('@')) { throw 'this install.ps1 has no pinned publisher (unreleased template)' }

$arch = switch ($env:PROCESSOR_ARCHITECTURE) {
  'AMD64' { 'x86_64' } 'ARM64' { 'aarch64' } default { throw "unsupported architecture $_" }
}
$target = "windows-$arch"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("genie-" + [guid]::NewGuid())
New-Item -ItemType Directory $tmp | Out-Null
try {
  $m = Invoke-RestMethod "$FeedsUrl/updates/$Channel/$target.json"
  if ([datetime]::Parse($m.expires_at).ToUniversalTime() -le [datetime]::UtcNow) {
    throw "manifest expired at $($m.expires_at) (stale mirror?)"
  }
  $a = $m.artifacts | Where-Object { $_.kind -eq 'cli' } | Select-Object -First 1
  if (-not $a) { throw "manifest has no CLI artifact for $target" }
  $zip = Join-Path $tmp 'genie.zip'
  Invoke-WebRequest $a.url -OutFile $zip -UseBasicParsing
  $sha = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLowerInvariant()
  if ($sha -ne $a.sha256) { throw "SHA-256 mismatch (expected $($a.sha256), got $sha)" }
  Expand-Archive $zip -DestinationPath $tmp
  $exe = Get-ChildItem $tmp -Recurse -Filter genie.exe | Select-Object -First 1
  $sig = Get-AuthenticodeSignature $exe.FullName
  if ($sig.Status -ne 'Valid') { throw "genie.exe Authenticode signature is $($sig.Status)" }
  if ($sig.SignerCertificate.GetNameInfo('SimpleName', $false) -ne $Publisher) {
    throw "genie.exe is signed by '$($sig.SignerCertificate.Subject)', not '$Publisher'"
  }
  New-Item -ItemType Directory -Force $Prefix | Out-Null
  Copy-Item $exe.FullName (Join-Path $Prefix 'genie.exe') -Force
  $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
  if (-not ($userPath -split ';' | Where-Object { $_ -eq $Prefix })) {
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$Prefix", 'User')
    Write-Host "added $Prefix to your user PATH (open a new terminal)"
  }
  & (Join-Path $Prefix 'genie.exe') --version
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
