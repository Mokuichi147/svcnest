[CmdletBinding()]
param(
    [string]$Version = 'latest',
    [string]$InstallDir = $env:SVCNEST_INSTALL_DIR,
    [switch]$NoModifyPath
)

function Get-SvcnestWindowsTarget {
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        throw 'このインストーラーは Windows x64 用です。'
    }
    $architecture = $env:PROCESSOR_ARCHITEW6432
    if (-not $architecture) { $architecture = $env:PROCESSOR_ARCHITECTURE }
    if ($architecture -ne 'AMD64') { throw "未対応の CPU: $architecture" }
    return 'x86_64-pc-windows-msvc'
}

function Get-SvcnestUserPath {
    return [Environment]::GetEnvironmentVariable('Path', 'User')
}

function Set-SvcnestUserPath([string]$Value) {
    [Environment]::SetEnvironmentVariable('Path', $Value, 'User')
}

function Add-SvcnestPath([string]$ExistingPath, [string]$Directory) {
    $normalized = [IO.Path]::GetFullPath($Directory).TrimEnd([char[]]'\/')
    foreach ($entry in ($ExistingPath -split ';')) {
        if (-not $entry.Trim()) { continue }
        try {
            $expanded = [Environment]::ExpandEnvironmentVariables($entry.Trim().Trim('"'))
            $candidate = [IO.Path]::GetFullPath($expanded).TrimEnd([char[]]'\/')
            if ([string]::Equals($candidate, $normalized, [StringComparison]::OrdinalIgnoreCase)) {
                return $ExistingPath
            }
        } catch { }
    }
    if ($ExistingPath) { return "$Directory;$ExistingPath" }
    return $Directory
}

function Update-SvcnestPath([string]$Directory) {
    $previous = Get-SvcnestUserPath
    $updated = Add-SvcnestPath $previous $Directory
    if ($updated -cne $previous) { Set-SvcnestUserPath $updated }
    # 現在の PowerShell でも、再起動せずにコマンドを使用できるようにする。
    $env:Path = Add-SvcnestPath $env:Path $Directory
}

function New-SvcnestTempDirectory {
    $path = Join-Path ([IO.Path]::GetTempPath()) ('svcnest-install-' + [Guid]::NewGuid().ToString('N'))
    [IO.Directory]::CreateDirectory($path) | Out-Null
    return $path
}

function Receive-SvcnestFile([Uri]$Uri, [string]$Destination) {
    Add-Type -AssemblyName System.Net.Http
    $previousTls = [Net.ServicePointManager]::SecurityProtocol
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $handler = [Net.Http.HttpClientHandler]::new()
    $handler.AllowAutoRedirect = $false
    if ($handler.PSObject.Properties.Name -contains 'SslProtocols') {
        $handler.SslProtocols = [Security.Authentication.SslProtocols]::Tls12
    }
    $client = [Net.Http.HttpClient]::new($handler)
    $client.Timeout = [TimeSpan]::FromSeconds(120)
    try {
        for ($redirect = 0; $redirect -le 5; $redirect++) {
            if ($Uri.Scheme -ne 'https') { throw 'HTTPS 以外へのダウンロードは許可しません。' }
            $response = $client.GetAsync($Uri).GetAwaiter().GetResult()
            try {
                $status = [int]$response.StatusCode
                if ($status -ge 300 -and $status -lt 400 -and $response.Headers.Location) {
                    $Uri = [Uri]::new($Uri, $response.Headers.Location)
                    continue
                }
                $response.EnsureSuccessStatusCode() | Out-Null
                $bytes = $response.Content.ReadAsByteArrayAsync().GetAwaiter().GetResult()
                [IO.File]::WriteAllBytes($Destination, $bytes)
                return
            } finally { $response.Dispose() }
        }
        throw 'リダイレクト回数が上限を超えました。'
    } finally {
        $client.Dispose()
        [Net.ServicePointManager]::SecurityProtocol = $previousTls
    }
}

function Get-SvcnestBinaryVersion([string]$Binary) {
    $output = & $Binary --version
    if ($LASTEXITCODE -ne 0) { throw '取得したバイナリを実行できませんでした。' }
    return ([string]$output).Trim()
}

function Install-Svcnest {
    [CmdletBinding()]
    param(
        [string]$Version = 'latest',
        [string]$InstallDir = $env:SVCNEST_INSTALL_DIR,
        [switch]$NoModifyPath
    )
    $ErrorActionPreference = 'Stop'
    $target = Get-SvcnestWindowsTarget
    if (-not $InstallDir) {
        $InstallDir = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Programs\svcnest'
    }
    $InstallDir = [IO.Path]::GetFullPath($InstallDir)
    if (-not $NoModifyPath -and $InstallDir -match '[;\r\n]') {
        throw 'PATH に追加できない配置先です。-NoModifyPath を指定できます。'
    }
    $repository = 'https://github.com/Mokuichi147/svcnest'
    if ($Version -eq 'latest') {
        $baseUrl = "$repository/releases/latest/download"
    } else {
        $Version = $Version -replace '^v', ''
        if ($Version -notmatch '^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$') {
            throw "無効なバージョン: $Version"
        }
        $baseUrl = "$repository/releases/download/v$Version"
    }
    $temporary = New-SvcnestTempDirectory
    $staging = $null
    try {
        $archiveName = "svcnest-$target.zip"
        $archive = Join-Path $temporary $archiveName
        $checksum = "$archive.sha256"
        Write-Host "svcnest を取得しています: $Version ($target)"
        Receive-SvcnestFile "$baseUrl/$archiveName" $archive
        Receive-SvcnestFile "$baseUrl/$archiveName.sha256" $checksum
        $record = [IO.File]::ReadAllText($checksum).Trim()
        $match = [regex]::Match($record, '^([0-9a-fA-F]{64})\s+' + [regex]::Escape($archiveName) + '$')
        if (-not $match.Success) { throw 'チェックサムの形式が不正です。' }
        $actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash
        if ($actual -ne $match.Groups[1].Value) { throw 'SHA-256 が一致しません。インストールを中止しました。' }

        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $package = [IO.Compression.ZipFile]::OpenRead($archive)
        $binary = Join-Path $temporary 'svcnest.exe'
        try {
            $entry = $package.GetEntry('svcnest.exe')
            if (-not $entry) { throw 'アーカイブに svcnest.exe がありません。' }
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $binary, $false)
        } finally { $package.Dispose() }
        $installedVersion = Get-SvcnestBinaryVersion $binary
        if ($installedVersion -notmatch '^svcnest \S+$') { throw 'バイナリのバージョン表示が不正です。' }
        if ($Version -ne 'latest' -and $installedVersion -cne "svcnest $Version") {
            throw '指定したバージョンとバイナリが一致しません。'
        }

        [IO.Directory]::CreateDirectory($InstallDir) | Out-Null
        $destination = Join-Path $InstallDir 'svcnest.exe'
        if (Test-Path -LiteralPath $destination) {
            $item = Get-Item -LiteralPath $destination -Force
            if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
                throw '配置先がディレクトリまたはシンボリックリンクです。'
            }
        }
        # 同じファイルシステムに完全なコピーを作り、失敗時は既存の exe を保持する。
        $staging = Join-Path $InstallDir ('.svcnest-install-' + [Guid]::NewGuid().ToString('N'))
        [IO.Directory]::CreateDirectory($staging) | Out-Null
        $stagedBinary = Join-Path $staging 'svcnest.exe'
        [IO.File]::Copy($binary, $stagedBinary)
        if ([IO.File]::Exists($destination)) {
            [IO.File]::Replace($stagedBinary, $destination, [NullString]::Value)
        } else {
            [IO.File]::Move($stagedBinary, $destination)
        }
        if (-not $NoModifyPath) { Update-SvcnestPath $InstallDir }
        Write-Host "インストール完了: $installedVersion"
        Write-Host "配置先: $destination"
    } finally {
        if ($staging -and [IO.Directory]::Exists($staging)) { [IO.Directory]::Delete($staging, $true) }
        if ([IO.Directory]::Exists($temporary)) { [IO.Directory]::Delete($temporary, $true) }
    }
}

# dot-source は検証・カスタム引数用に定義だけを読み込み、通常の実行は最後に開始する。
if ($MyInvocation.InvocationName -ne '.') {
    Install-Svcnest -Version $Version -InstallDir $InstallDir -NoModifyPath:$NoModifyPath
}
