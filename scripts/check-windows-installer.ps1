[CmdletBinding()]
param([string]$Binary)

$ErrorActionPreference = 'Stop'
$source = Split-Path $PSScriptRoot -Parent
. (Join-Path $source 'install.ps1')
# Windows PowerShell 5.1 では ZipArchiveMode の定義元も明示的に読み込む。
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$root = Join-Path ([IO.Path]::GetTempPath()) ('svcnest-windows-installer-check-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($root) | Out-Null
$assets = Join-Path $root 'assets'
[IO.Directory]::CreateDirectory($assets) | Out-Null
$install = Join-Path $root 'install with spaces'
$destination = Join-Path $install 'svcnest.exe'
$archiveName = 'svcnest-x86_64-pc-windows-msvc.zip'
$archive = Join-Path $assets $archiveName
$checksum = "$archive.sha256"
$script:Checks = 0
$script:Requests = @()
$script:ExpectedBase = 'https://github.com/Mokuichi147/svcnest/releases/latest/download'
$script:UserPath = 'existing-user-path'
$script:PathWrites = 0
$previousProcessPath = $env:Path
$previousProbePath = $env:SVCNEST_TEST_BIN
$realBinaryVersion = (Get-Command Get-SvcnestBinaryVersion).ScriptBlock
$realTarget = (Get-Command Get-SvcnestWindowsTarget).ScriptBlock
$realReceive = (Get-Command Receive-SvcnestFile).ScriptBlock
$realFind = (Get-Command Find-SvcnestExecutable).ScriptBlock
$realUserPathGet = (Get-Command Get-SvcnestUserPath).ScriptBlock
$realUserPathSet = (Get-Command Set-SvcnestUserPath).ScriptBlock
$manifest = [IO.File]::ReadAllText((Join-Path $source 'Cargo.toml'))
$packageVersion = [regex]::Match($manifest, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value

# 通信とユーザー環境変数は模擬処理に置き換え、実際のレジストリは変更しない。
function Get-SvcnestWindowsTarget { return 'x86_64-pc-windows-msvc' }
function Get-SvcnestUserPath { return $script:UserPath }
function Set-SvcnestUserPath([string]$Value) { $script:UserPath = $Value; $script:PathWrites++ }
function New-SvcnestTempDirectory {
    $path = Join-Path $root ('download-' + [Guid]::NewGuid().ToString('N'))
    [IO.Directory]::CreateDirectory($path) | Out-Null
    return $path
}
function Receive-SvcnestFile([Uri]$Uri, [string]$Destination) {
    if (-not $Uri.AbsoluteUri.StartsWith("$script:ExpectedBase/")) { throw "取得先が不正です: $Uri" }
    $script:Requests += $Uri.AbsoluteUri
    $name = $Uri.Segments[-1]
    [IO.File]::Copy((Join-Path $assets $name), $Destination)
}
function Get-SvcnestBinaryVersion([string]$Binary) {
    $value = [IO.File]::ReadAllText($Binary)
    if ($value -eq 'cannot execute') { throw '取得したバイナリを実行できませんでした。' }
    return $value
}

function Assert-Installer([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
    $script:Checks++
}

function New-Fixture([byte[]]$Contents, [string]$EntryName = 'svcnest.exe') {
    if ([IO.File]::Exists($archive)) { [IO.File]::Delete($archive) }
    $package = [IO.Compression.ZipFile]::Open($archive, [IO.Compression.ZipArchiveMode]::Create)
    try {
        $entry = $package.CreateEntry($EntryName)
        $stream = $entry.Open()
        try { $stream.Write($Contents, 0, $Contents.Length) } finally { $stream.Dispose() }
    } finally { $package.Dispose() }
    $digest = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText($checksum, "$digest  $archiveName`n", [Text.Encoding]::ASCII)
}

function Invoke-TestInstall([string]$Version = 'latest', [switch]$ModifyPath) {
    Install-Svcnest -Version $Version -InstallDir $install -NoModifyPath:(-not $ModifyPath)
    Assert-Installer (@(Get-ChildItem -LiteralPath $root -Filter 'download-*').Count -eq 0) '取得用の一時ディレクトリが残っています。'
    Assert-Installer (@(Get-ChildItem -LiteralPath $install -Filter '.svcnest-install-*').Count -eq 0) '配置用の一時ディレクトリが残っています。'
}

function Expect-InstallFailure([string]$Pattern, [string]$Version = 'latest') {
    $before = [IO.File]::ReadAllBytes($destination)
    $pathBefore = $script:UserPath
    $failure = $null
    try { Invoke-TestInstall -Version $Version } catch { $failure = $_ }
    Assert-Installer ($null -ne $failure) '失敗すべきインストールが成功しました。'
    if ($Pattern) { Assert-Installer ($failure.Exception.Message -match $Pattern) $failure.Exception.Message }
    Assert-Installer ([Convert]::ToBase64String($before) -eq [Convert]::ToBase64String([IO.File]::ReadAllBytes($destination))) '既存の実行ファイルを変更しました。'
    Assert-Installer ($script:UserPath -ceq $pathBefore) '失敗時にユーザー PATH を変更しました。'
    Assert-Installer (@(Get-ChildItem -LiteralPath $root -Filter 'download-*').Count -eq 0) '失敗時に一時ディレクトリが残っています。'
}

try {
    $failure = $null
    try { & $realReceive -Uri 'http://example.invalid/archive.zip' -Destination (Join-Path $root 'never-downloaded') } catch { $failure = $_ }
    Assert-Installer ($null -ne $failure -and $failure.Exception.Message -match 'HTTPS') 'HTTPS 以外を拒否できません。'
    Assert-Installer (-not [IO.File]::Exists((Join-Path $root 'never-downloaded'))) 'HTTPS 以外の取得でファイルを作成しました。'
    $first = [Text.Encoding]::UTF8.GetBytes("svcnest $packageVersion")
    New-Fixture $first
    Invoke-TestInstall
    Assert-Installer ([IO.File]::ReadAllText($destination) -ceq "svcnest $packageVersion") '最新安定版を配置できません。'
    Assert-Installer ($script:PathWrites -eq 0 -and $env:Path -ceq $previousProcessPath) '-NoModifyPath でも PATH を変更しました。'

    $script:ExpectedBase = "https://github.com/Mokuichi147/svcnest/releases/download/v$packageVersion"
    Invoke-TestInstall -Version $packageVersion
    Invoke-TestInstall -Version "v$packageVersion"
    $script:ExpectedBase = 'https://github.com/Mokuichi147/svcnest/releases/latest/download'

    # 開いている旧ファイルの内容も、更新によって上書きされないことを確認する。
    $previous = [IO.File]::Open($destination, [IO.FileMode]::Open, [IO.FileAccess]::Read, ([IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete))
    try {
        New-Fixture ([Text.Encoding]::UTF8.GetBytes('svcnest 0.0.0'))
        Invoke-TestInstall
        $buffer = New-Object byte[] $first.Length
        $count = $previous.Read($buffer, 0, $buffer.Length)
        Assert-Installer ($count -eq $first.Length -and [Convert]::ToBase64String($buffer) -eq [Convert]::ToBase64String($first)) '開いている旧ファイルを上書きしました。'
    } finally { $previous.Dispose() }

    New-Fixture $first
    [IO.File]::AppendAllText($archive, 'tampered')
    Expect-InstallFailure 'SHA-256'
    New-Fixture $first
    [IO.File]::WriteAllText($checksum, 'invalid checksum')
    Expect-InstallFailure '形式が不正'
    New-Fixture $first
    [IO.File]::Delete($checksum)
    Expect-InstallFailure ''
    New-Fixture ([Text.Encoding]::UTF8.GetBytes('cannot execute'))
    Expect-InstallFailure '実行できません'
    New-Fixture ([Text.Encoding]::UTF8.GetBytes('invalid version'))
    Expect-InstallFailure 'バージョン表示が不正'
    New-Fixture $first -EntryName 'other.exe'
    Expect-InstallFailure 'アーカイブに svcnest.exe'
    New-Fixture ([Text.Encoding]::UTF8.GetBytes('svcnest 0.0.0'))
    $script:ExpectedBase = "https://github.com/Mokuichi147/svcnest/releases/download/v$packageVersion"
    Expect-InstallFailure 'バージョンとバイナリが一致' -Version $packageVersion
    $script:ExpectedBase = 'https://github.com/Mokuichi147/svcnest/releases/latest/download'
    $requestCount = $script:Requests.Count
    Expect-InstallFailure '無効なバージョン' -Version '../../other'
    Assert-Installer ($script:Requests.Count -eq $requestCount) '無効なバージョンでも通信しました。'

    New-Fixture $first
    [IO.File]::Delete($destination)
    [IO.Directory]::CreateDirectory($destination) | Out-Null
    $failure = $null
    try { Invoke-TestInstall } catch { $failure = $_ }
    Assert-Installer ($null -ne $failure -and $failure.Exception.Message -match 'ディレクトリ') '配置先のディレクトリを拒否しませんでした。'
    [IO.Directory]::Delete($destination)

    Invoke-TestInstall -ModifyPath
    Assert-Installer ($script:UserPath -ceq "$install;existing-user-path") '既存のユーザー PATH を保持できません。'
    Assert-Installer (($env:Path -split ';')[0] -ceq $install) '現在の PowerShell に PATH を反映できません。'
    $writes = $script:PathWrites
    Invoke-TestInstall -ModifyPath
    Assert-Installer ($script:PathWrites -eq $writes) '同じユーザー PATH を重複して書き込みました。'
    Assert-Installer (@($env:Path -split ';' | Where-Object { $_ -ceq $install }).Count -eq 1) '現在の PATH に配置先が重複しました。'
    $env:SVCNEST_TEST_BIN = $install
    $rawPath = '%SVCNEST_TEST_BIN%;existing-user-path'
    Assert-Installer ((Add-SvcnestPath $rawPath $install) -ceq $rawPath) '既存 PATH 内の環境変数表記を展開して保存しました。'
    Assert-Installer ((Add-SvcnestPath ($install.ToUpperInvariant() + [IO.Path]::DirectorySeparatorChar) $install) -ceq ($install.ToUpperInvariant() + [IO.Path]::DirectorySeparatorChar)) '大文字や末尾区切りの違いで PATH を重複しました。'

    # 既存 Cargo の配置先を使い、自動起動の参照先を変えずに更新する。
    $cargo = Join-Path $root 'legacy cargo'
    $legacyDirectory = Join-Path $cargo 'bin'
    [IO.Directory]::CreateDirectory($legacyDirectory) | Out-Null
    $script:LegacyExecutable = Join-Path $legacyDirectory 'svcnest.exe'
    [IO.File]::WriteAllText($script:LegacyExecutable, 'svcnest 0.9.0')
    function Find-SvcnestExecutable { return $script:LegacyExecutable }
    Install-Svcnest -InstallDir '' -NoModifyPath
    Assert-Installer ([IO.File]::ReadAllText($script:LegacyExecutable) -ceq "svcnest $packageVersion") '既存の Cargo 配置先を更新できません。'
    Assert-Installer ((Resolve-SvcnestInstallDirectory $install) -ceq $install) '明示した配置先を優先しませんでした。'
    [IO.File]::WriteAllText($script:LegacyExecutable, 'svcnest 0.9.0')
    Invoke-TestInstall
    Assert-Installer ([IO.File]::ReadAllText($script:LegacyExecutable) -ceq 'svcnest 0.9.0') '明示的な別配置先への導入で既存 CLI を変更しました。'
    $previousCargoHome = $env:CARGO_HOME
    $lookupPath = $env:Path
    try {
        $env:CARGO_HOME = $cargo
        $env:Path = ''
        Assert-Installer ((& $realFind) -ceq $script:LegacyExecutable) 'PATH にない既存 Cargo 配置先を見つけられません。'
        if ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT) {
            $env:Path = "$legacyDirectory;$lookupPath"
            Assert-Installer ((& $realFind) -ceq $script:LegacyExecutable) 'PATH 内の既存 CLI を見つけられません。'
        }
    } finally { $env:CARGO_HOME = $previousCargoHome; $env:Path = $lookupPath }

    if ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT) {
        # 実ユーザーの Environment ではなく、隔離した実レジストリで getter / setter を検証する。
        $script:RegistryTestPath = 'Software\svcnest\InstallerTests\' + [Guid]::NewGuid().ToString('N')
        $script:Notifications = 0
        function Open-SvcnestUserEnvironment([switch]$Writable) {
            if ($Writable) { return [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($script:RegistryTestPath) }
            return [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($script:RegistryTestPath)
        }
        function Send-SvcnestEnvironmentChange { $script:Notifications++ }
        $mockGet = (Get-Command Get-SvcnestUserPath).ScriptBlock
        $mockSet = (Get-Command Set-SvcnestUserPath).ScriptBlock
        $previousJava = $env:SVCNEST_TEST_JAVA_HOME
        $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($script:RegistryTestPath)
        try {
            Set-Item Function:Get-SvcnestUserPath $realUserPathGet
            Set-Item Function:Set-SvcnestUserPath $realUserPathSet
            $env:SVCNEST_TEST_JAVA_HOME = Join-Path $root 'jdk17'
            $raw = '%SVCNEST_TEST_JAVA_HOME%\bin;existing-user-path'
            $key.SetValue('Path', $raw, [Microsoft.Win32.RegistryValueKind]::ExpandString)
            Assert-Installer ((Get-SvcnestUserPath) -ceq $raw) 'getter が PATH の変数参照を展開しました。'
            Update-SvcnestPath $install
            $expected = "$install;$raw"
            Assert-Installer ((Get-SvcnestUserPath) -ceq $expected) 'PATH の変数参照を保存時に変更しました。'
            Assert-Installer ($key.GetValueKind('Path') -eq [Microsoft.Win32.RegistryValueKind]::ExpandString) 'REG_EXPAND_SZ を保持しませんでした。'
            $env:SVCNEST_TEST_JAVA_HOME = Join-Path $root 'jdk21'
            Assert-Installer ($key.GetValue('Path') -ceq [Environment]::ExpandEnvironmentVariables($expected)) 'PATH が新しい環境変数の値に追従しませんでした。'
            $notifications = $script:Notifications
            Update-SvcnestPath $install
            Assert-Installer ($script:Notifications -eq $notifications) '同じ PATH を重複して保存しました。'
            $key.SetValue('Path', $raw, [Microsoft.Win32.RegistryValueKind]::String)
            Update-SvcnestPath $install
            Assert-Installer ($key.GetValueKind('Path') -eq [Microsoft.Win32.RegistryValueKind]::String) 'REG_SZ の種類を変更しました。'
            Assert-Installer ($key.GetValue('Path') -ceq $expected) 'REG_SZ の未展開値を変更しました。'
            $key.DeleteValue('Path')
            Update-SvcnestPath $install
            Assert-Installer ((Get-SvcnestUserPath) -ceq $install) 'ユーザー PATH を新規作成できません。'
            Assert-Installer ($key.GetValueKind('Path') -eq [Microsoft.Win32.RegistryValueKind]::ExpandString) '新規 PATH に展開可能な種類を指定しませんでした。'
        } finally {
            Set-Item Function:Get-SvcnestUserPath $mockGet
            Set-Item Function:Set-SvcnestUserPath $mockSet
            $env:SVCNEST_TEST_JAVA_HOME = $previousJava
            $key.Dispose()
            [Microsoft.Win32.Registry]::CurrentUser.DeleteSubKeyTree($script:RegistryTestPath)
        }
    }

    if ($Binary) {
        # Windows CI では実際の配布用 PE を梱包し、配置後に実行する。
        Set-Item Function:Get-SvcnestBinaryVersion $realBinaryVersion
        Set-Item Function:Get-SvcnestWindowsTarget $realTarget
        $native = [IO.Path]::GetFullPath($Binary)
        New-Fixture ([IO.File]::ReadAllBytes($native))
        Invoke-TestInstall
        $actualVersion = & $destination --version
        Assert-Installer ($LASTEXITCODE -eq 0 -and $actualVersion -ceq "svcnest $packageVersion") '配置した実際の Windows バイナリを実行できません。'
        Assert-Installer ((Get-FileHash -LiteralPath $native).Hash -eq (Get-FileHash -LiteralPath $destination).Hash) '配布バイナリと配置後の内容が一致しません。'
        [IO.File]::Copy($native, $script:LegacyExecutable, $true)
        $definition = & $script:LegacyExecutable --home (Join-Path $root 'registered state') daemon install --dry-run
        Install-Svcnest -InstallDir '' -NoModifyPath
        $after = & $script:LegacyExecutable --home (Join-Path $root 'registered state') daemon install --dry-run
        Assert-Installer (($definition -join "`n") -ceq ($after -join "`n")) '更新で自動起動の参照先が変わりました。'
        Assert-Installer ((Get-FileHash -LiteralPath $native).Hash -eq (Get-FileHash -LiteralPath $script:LegacyExecutable).Hash) '既存の配置先のバイナリを更新できません。'
    }

    # Invoke-Expression に渡した場合にも最後の処理が一度だけ開始することを確認する。
    $bootstrap = [IO.File]::ReadAllText((Join-Path $source 'install.ps1'))
    $tokens = $null
    $parseErrors = $null
    $syntax = [Management.Automation.Language.Parser]::ParseInput($bootstrap, [ref]$tokens, [ref]$parseErrors)
    if ($parseErrors.Count) { throw $parseErrors[0].Message }
    $entryFunction = $syntax.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Install-Svcnest' }, $true)
    $fakeEntry = 'function Install-Svcnest { param([string]$Version, [string]$InstallDir, [switch]$NoModifyPath); $script:BootstrapCalls++; $script:BootstrapDirectory = $InstallDir; $script:BootstrapVersion = $Version; $script:BootstrapNoModifyPath = $NoModifyPath.IsPresent }'
    $bootstrap = $bootstrap.Substring(0, $entryFunction.Extent.StartOffset) + $fakeEntry + $bootstrap.Substring($entryFunction.Extent.EndOffset)
    $script:BootstrapCalls = 0
    $previousInstallDir = $env:SVCNEST_INSTALL_DIR
    try {
        $env:SVCNEST_INSTALL_DIR = $install
        Invoke-Expression $bootstrap
        Assert-Installer ($script:BootstrapCalls -eq 1) 'Invoke-Expression からインストールが開始しませんでした。'
        Assert-Installer ($script:BootstrapDirectory -ceq $install) 'Invoke-Expression から配置先を引き継げませんでした。'
        $script:BootstrapCalls = 0
        & ([scriptblock]::Create($bootstrap)) -Version '1.2.3' -InstallDir $install -NoModifyPath
        Assert-Installer ($script:BootstrapCalls -eq 1) '引数付きのスクリプトからインストールが開始しませんでした。'
        Assert-Installer ($script:BootstrapVersion -ceq '1.2.3') 'スクリプトからバージョンを引き継げませんでした。'
        Assert-Installer ($script:BootstrapDirectory -ceq $install) 'スクリプトから配置先を引き継げませんでした。'
        Assert-Installer $script:BootstrapNoModifyPath 'スクリプトから PATH の自動設定無効化を引き継げませんでした。'
    } finally { $env:SVCNEST_INSTALL_DIR = $previousInstallDir }
    Write-Host "Windows インストーラー検証成功: $script:Checks 件"
} finally {
    $env:Path = $previousProcessPath
    $env:SVCNEST_TEST_BIN = $previousProbePath
    if ([IO.Directory]::Exists($root)) { [IO.Directory]::Delete($root, $true) }
}
