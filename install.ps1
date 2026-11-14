$ErrorActionPreference = "Stop"
$Repo = "proxysoul/stem"
$Target = switch ($env:PROCESSOR_ARCHITECTURE) {
  "AMD64" { "x86_64-pc-windows-msvc" }
  "ARM64" { "aarch64-pc-windows-msvc" }
  default { throw "stem: no prebuilt binary for Windows $($env:PROCESSOR_ARCHITECTURE); build from source with: cargo install --git https://github.com/$Repo stem" }
}
$Release = if ($env:STEM_VERSION) { "download/$($env:STEM_VERSION)" } else { "latest/download" }
$Url = "https://github.com/$Repo/releases/$Release/stem-$Target.tar.gz"
$Dir = if ($env:STEM_INSTALL_DIR) { $env:STEM_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "Programs\stem" }
$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("stem-" + [System.Guid]::NewGuid())
New-Item -ItemType Directory -Path $Tmp | Out-Null
try {
  $Archive = Join-Path $Tmp "stem.tar.gz"
  Invoke-WebRequest -Uri $Url -OutFile $Archive -UseBasicParsing
  $Expected = ((Invoke-WebRequest -Uri "$Url.sha256" -UseBasicParsing).Content -split "\s+")[0].ToLower()
  $Actual = (Get-FileHash -Algorithm SHA256 $Archive).Hash.ToLower()
  if ($Expected -ne $Actual) { throw "stem: checksum mismatch, not installing" }
  tar -xzf $Archive -C $Tmp
  New-Item -ItemType Directory -Force -Path $Dir | Out-Null
  Copy-Item -Force (Join-Path $Tmp "stem.exe") (Join-Path $Dir "stem.exe")
  Write-Output "stem installed to $(Join-Path $Dir 'stem.exe')"
  $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
  if (-not (($UserPath -split ";") -contains $Dir)) {
    [Environment]::SetEnvironmentVariable("Path", (@($UserPath, $Dir) | Where-Object { $_ }) -join ";", "User")
    Write-Output "added $Dir to your PATH; open a new terminal to use stem"
  }
} finally {
  Remove-Item -Recurse -Force $Tmp
}
