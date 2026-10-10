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

function Open-SvcnestUserEnvironment([switch]$Writable) {
    if ($Writable) { return [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment') }
    return [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment')
}

function Get-SvcnestUserPath {
    $key = Open-SvcnestUserEnvironment
    if (-not $key) { return $null }
    try {
        return $key.GetValue('Path', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    } finally { $key.Dispose() }
}

function Send-SvcnestEnvironmentChange {
    if (-not ('Svcnest.Installer.EnvironmentNotification' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace Svcnest.Installer {
    public static class EnvironmentNotification {
        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern IntPtr SendMessageTimeout(IntPtr window, uint message,
            UIntPtr wParam, string lParam, uint flags, uint timeout, out UIntPtr result);
        public static void Send() {
            UIntPtr result;
            SendMessageTimeout(new IntPtr(0xffff), 0x001a, UIntPtr.Zero,
                "Environment", 2, 1000, out result);
        }
    }
}
'@
    }
    [Svcnest.Installer.EnvironmentNotification]::Send()
}

function Set-SvcnestUserPath([string]$Value) {
    $key = Open-SvcnestUserEnvironment -Writable
    try {
        $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
        if ($key.GetValueNames() -contains 'Path') { $kind = $key.GetValueKind('Path') }
        # 既存の変数参照と REG_SZ / REG_EXPAND_SZ の種類をそのまま保持する。
        $key.SetValue('Path', $Value, $kind)
    } finally { $key.Dispose() }
    Send-SvcnestEnvironmentChange
}

function Find-SvcnestExecutable {
    $candidates = @(Get-Command svcnest.exe -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1 | ForEach-Object { $_.Path })
    $cargo = $env:CARGO_HOME
    if (-not $cargo) { $cargo = Join-Path ([Environment]::GetFolderPath('UserProfile')) '.cargo' }
    $candidate = Join-Path $cargo 'bin\svcnest.exe'
    if ([IO.File]::Exists($candidate) -and $candidates -notcontains $candidate) { $candidates += $candidate }
    return $candidates
}

function Test-SvcnestUpdatableExecutable([string]$Path) {
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    if (-not $item -or $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) { return $false }
    # Program Files などの書き込めない場所は、管理者権限なしでは更新できない。
    $probe = Join-Path $item.DirectoryName ('.svcnest-write-test-' + [Guid]::NewGuid().ToString('N'))
    try {
        [IO.File]::WriteAllBytes($probe, [byte[]]@())
        [IO.File]::Delete($probe)
        return $true
    } catch { return $false }
}

function Resolve-SvcnestInstallDirectory([string]$Requested) {
    if (-not $Requested) {
        # 自動起動が参照する既存 CLI の配置先を維持する。
        foreach ($existing in @(Find-SvcnestExecutable)) {
            if (Test-SvcnestUpdatableExecutable $existing) {
                $Requested = [IO.Path]::GetDirectoryName($existing)
                break
            }
            if (-not $script:SvcnestSkippedExecutable) { $script:SvcnestSkippedExecutable = $existing }
        }
        if (-not $Requested) {
            $Requested = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Programs\svcnest'
        }
    }
    return [IO.Path]::GetFullPath($Requested)
}

function Remove-SvcnestRetiredExecutable([string]$Directory) {
    # 実行中だった旧 exe は削除できないため、次回の更新時に改めて削除する。
    foreach ($retired in [IO.Directory]::GetFiles($Directory, '.svcnest-old-*.exe')) {
        try { [IO.File]::Delete($retired) } catch { }
    }
}

function Install-SvcnestExecutable([string]$Source, [string]$Destination) {
    if (-not [IO.File]::Exists($Destination)) {
        [IO.File]::Move($Source, $Destination)
        return
    }
    # 実行中の exe は上書き・削除できないが改名はできるため、退避してから置き換える。
    $retired = Join-Path ([IO.Path]::GetDirectoryName($Destination)) ('.svcnest-old-' + [Guid]::NewGuid().ToString('N') + '.exe')
    [IO.File]::Move($Destination, $retired)
    try {
        [IO.File]::Move($Source, $Destination)
    } catch {
        [IO.File]::Move($retired, $Destination)
        throw
    }
    Remove-SvcnestRetiredExecutable ([IO.Path]::GetDirectoryName($Destination))
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
    $script:SvcnestSkippedExecutable = $null
    $InstallDir = Resolve-SvcnestInstallDirectory $InstallDir
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
        Install-SvcnestExecutable $stagedBinary $destination
        if (-not $NoModifyPath) { Update-SvcnestPath $InstallDir }
        Write-Host "インストール完了: $installedVersion"
        Write-Host "配置先: $destination"
        if ($script:SvcnestSkippedExecutable) {
            Write-Warning "シンボリックリンクまたは書き込めない場所にある既存の $($script:SvcnestSkippedExecutable) は更新していません。PATH の順序によっては既存のコマンドが優先されます。"
        }
    } finally {
        if ($staging -and [IO.Directory]::Exists($staging)) { [IO.Directory]::Delete($staging, $true) }
        if ([IO.Directory]::Exists($temporary)) { [IO.Directory]::Delete($temporary, $true) }
    }
}

# dot-source は検証・カスタム引数用に定義だけを読み込み、通常の実行は最後に開始する。
if ($MyInvocation.InvocationName -ne '.') {
    Install-Svcnest -Version $Version -InstallDir $InstallDir -NoModifyPath:$NoModifyPath
}
