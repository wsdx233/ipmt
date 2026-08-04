$ErrorActionPreference = "Stop"
Set-StrictMode -Version 2.0

$Repository = "wsdx233/ipmt"
$Target = "x86_64-pc-windows-msvc"
$Archive = "ipmt-$Target.zip"
$Checksum = "ipmt-$Target.sha256"
$ReleaseUrl = "https://github.com/$Repository/releases/latest/download"

if ($env:IPMT_INSTALL_DIR) {
    $InstallDir = $env:IPMT_INSTALL_DIR
} else {
    $InstallDir = Join-Path $env:LOCALAPPDATA "Programs\ipmt\bin"
}

$TempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("ipmt-install-" + [guid]::NewGuid().ToString("N"))

try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    New-Item -ItemType Directory -Path $TempDir -Force | Out-Null

    $ArchivePath = Join-Path $TempDir $Archive
    $ChecksumPath = Join-Path $TempDir $Checksum

    Write-Host "正在下载 ipmt 最新版本（$Target）..."
    Invoke-WebRequest -UseBasicParsing -Uri "$ReleaseUrl/$Archive" -OutFile $ArchivePath
    Invoke-WebRequest -UseBasicParsing -Uri "$ReleaseUrl/$Checksum" -OutFile $ChecksumPath

    $ChecksumText = (Get-Content -Raw $ChecksumPath).Trim()
    $ChecksumMatch = [regex]::Match($ChecksumText, '(?i)\b[0-9a-f]{64}\b')
    if (-not $ChecksumMatch.Success) {
        throw "校验文件格式无效"
    }

    $ExpectedHash = $ChecksumMatch.Value.ToUpperInvariant()
    $ActualHash = (Get-FileHash -Algorithm SHA256 $ArchivePath).Hash.ToUpperInvariant()
    if ($ActualHash -ne $ExpectedHash) {
        throw "SHA-256 校验失败"
    }

    $ExtractDir = Join-Path $TempDir "extracted"
    Expand-Archive -Path $ArchivePath -DestinationPath $ExtractDir -Force
    $Executable = Get-ChildItem -Path $ExtractDir -Filter "ipmt.exe" -File -Recurse | Select-Object -First 1
    if (-not $Executable) {
        throw "发布包中没有找到 ipmt.exe"
    }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Copy-Item -Path $Executable.FullName -Destination (Join-Path $InstallDir "ipmt.exe") -Force

    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $PathEntries = @($UserPath -split ';' | Where-Object { $_ })
    if ($PathEntries -notcontains $InstallDir) {
        $NewUserPath = (@($PathEntries) + $InstallDir) -join ';'
        [Environment]::SetEnvironmentVariable("Path", $NewUserPath, "User")
        Write-Host "已将 $InstallDir 加入用户 PATH。"
    }

    if (($env:Path -split ';') -notcontains $InstallDir) {
        $env:Path = "$InstallDir;$env:Path"
    }

    Write-Host "安装完成：$(Join-Path $InstallDir 'ipmt.exe')"
    Write-Host "请重新打开终端，然后运行 ipmt。"
} catch {
    Write-Error "ipmt installer: $($_.Exception.Message)"
    exit 1
} finally {
    if (Test-Path $TempDir) {
        Remove-Item -Path $TempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}
