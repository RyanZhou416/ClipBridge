<#
.SYNOPSIS
    受控 Windows Authenticode 签名入口（项目自有 EXE/DLL/MSI；默认不签）。

.DESCRIPTION
    这是显式的、无隐藏降级的签名入口，供 scripts/build-windows-installers.ps1 和
    release worker 复用：
      - Mode None（默认）：不修改任何文件，明确返回“unsigned”。
      - Mode Certificate：使用 Windows 证书库中已有证书（thumbprint）调用 signtool，
        带 RFC3161 时间戳 URL，然后验证信任链、签名者 thumbprint 和时间戳。
    不会创建/导入证书，不会申请服务凭据，不会自动安装 Windows SDK，也不会
    blind resign 第三方 DLL；调用方只应传入项目自有载荷。

    退出码：
      0 成功（None 也为 0，但不代表可信签名）
      2 参数/配置错误（缺少 thumbprint、非法 URL、找不到 signtool、非法文件类型等）
      1 签名或验签失败

.PARAMETER Mode
    None 或 Certificate。默认 None。

.PARAMETER Files
    要签名的文件，按调用方给定顺序。仅接受 .exe/.dll/.msi；ZIP 不套 Authenticode。
    Certificate 模式下不能为空。

.PARAMETER CertificateThumbprint
    代码签名证书的 SHA1 thumbprint（40 位十六进制），证书必须已在 Windows 证书库中。
    本脚本不会导入或创建证书。

.PARAMETER TimestampUrl
    RFC3161 时间戳 URL，必须为 http/https（生产优先 https）。

.PARAMETER SigntoolPath
    可选的 signtool.exe 绝对路径；不传时从 Windows Kits 10 的 x64 工具目录中查找。
    不会下载或安装。

.PARAMETER ValidateOnly
    只验证参数、证书 thumbprint 格式、时间戳 URL 和 signtool 可用性，不签名。
    供构建脚本在重型构建前 fail-fast。

.EXAMPLE
    pwsh scripts/ci/sign-windows.ps1 -Mode None -Files dist/a.msi

.EXAMPLE
    pwsh scripts/ci/sign-windows.ps1 -Mode Certificate `
        -CertificateThumbprint 0123456789ABCDEF0123456789ABCDEF01234567 `
        -TimestampUrl https://timestamp.example.invalid/rfc3161 `
        -Files dist/a.msi
#>
[CmdletBinding()]
param(
    [ValidateSet('None', 'Certificate')]
    [string]$Mode = 'None',
    [string[]]$Files,
    [string]$CertificateThumbprint,
    [string]$TimestampUrl,
    [string]$SigntoolPath,
    [switch]$ValidateOnly
)

$ErrorActionPreference = 'Stop'

function Resolve-SignTool {
    param([string]$ExplicitPath)
    if ($ExplicitPath) {
        if (-not (Test-Path -LiteralPath $ExplicitPath -PathType Leaf)) {
            throw [System.ArgumentException]::new("指定的 signtool.exe 不存在：$ExplicitPath")
        }
        if ((Split-Path -Leaf $ExplicitPath) -ine 'signtool.exe') {
            throw [System.ArgumentException]::new("指定的文件不是 signtool.exe：$ExplicitPath")
        }
        return (Resolve-Path -LiteralPath $ExplicitPath).Path
    }

    $roots = @()
    if (${env:ProgramFiles(x86)}) { $roots += (Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin') }
    if ($env:ProgramFiles) { $roots += (Join-Path $env:ProgramFiles 'Windows Kits\10\bin') }
    $roots = @($roots | Where-Object { $_ -and (Test-Path -LiteralPath $_) })

    $candidates = [System.Collections.Generic.List[object]]::new()
    foreach ($root in $roots) {
        foreach ($dir in @(Get-ChildItem -LiteralPath $root -Directory -ErrorAction SilentlyContinue)) {
            $candidate = Join-Path $dir.FullName 'x64\signtool.exe'
            if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                $candidates.Add([pscustomobject]@{ Path = $candidate; Version = $dir.Name })
            }
        }
    }
    if ($candidates.Count -eq 0) {
        throw [System.ArgumentException]::new(
            '找不到 signtool.exe（Windows SDK 10 x64 工具）。请安装 Windows SDK 或用 -SigntoolPath 指定；本脚本不会自动安装。')
    }
    $best = $candidates | Sort-Object -Property @{ Expression = { try { [version]$_.Version } catch { [version]'0.0.0.0' } } } -Descending | Select-Object -First 1
    return [string]$best.Path
}

function Test-TimestampUrl {
    param([string]$Url)
    if ([string]::IsNullOrWhiteSpace($Url)) {
        throw [System.ArgumentException]::new('Certificate 模式必须提供 -TimestampUrl（RFC3161 https URL）。')
    }
    $uri = $null
    if (-not [System.Uri]::TryCreate($Url, [System.UriKind]::Absolute, [ref]$uri)) {
        throw [System.ArgumentException]::new("TimestampUrl 不是绝对 URL：$Url")
    }
    if ($uri.Scheme -notin @('http', 'https')) {
        throw [System.ArgumentException]::new("TimestampUrl 必须使用 http/https：$Url")
    }
    if ($uri.Scheme -eq 'http') {
        Write-Warning "TimestampUrl 使用 http：时间戳 token 仍由 TSA 签名，但传输不加密；生产环境优先 https。"
    }
}

function Assert-ValidThumbprint {
    param([string]$Thumbprint)
    if ([string]::IsNullOrWhiteSpace($Thumbprint)) {
        throw [System.ArgumentException]::new('Certificate 模式必须提供 -CertificateThumbprint（证书库中已有证书的 SHA1 thumbprint）。')
    }
    if ($Thumbprint -notmatch '^[0-9A-Fa-f]{40}$') {
        throw [System.ArgumentException]::new("CertificateThumbprint 必须是 40 位十六进制 SHA1 thumbprint：$Thumbprint")
    }
}

$exitCode = 0
try {
    if ($Mode -eq 'None') {
        foreach ($name in @('CertificateThumbprint', 'TimestampUrl', 'SigntoolPath')) {
            if ($PSBoundParameters.ContainsKey($name) -and -not [string]::IsNullOrWhiteSpace([string]$PSBoundParameters[$name])) {
                throw [System.ArgumentException]::new(
                    "Mode=None 时不能提供 -$name；None 只用于 CI/draft/明确预发布，不能伪装成可信生产签名。")
            }
        }
        $count = if ($Files) { @($Files).Count } else { 0 }
        Write-Host "[sign] Mode=None：未签名（$count 个文件未修改）。该结果不是 trusted signature，只允许用于 CI/draft/明确接受的预发布。"
        exit 0
    }

    Assert-ValidThumbprint -Thumbprint $CertificateThumbprint
    Test-TimestampUrl -Url $TimestampUrl
    $signtool = Resolve-SignTool -ExplicitPath $SigntoolPath
    Write-Host "[sign] signtool: $signtool"
    Write-Host "[sign] certificate thumbprint: $CertificateThumbprint"
    Write-Host "[sign] timestamp: $TimestampUrl"

    if ($ValidateOnly) {
        Write-Host '[sign] 配置验证通过（未签名）。'
        exit 0
    }

    if (-not $Files -or @($Files).Count -eq 0) {
        throw [System.ArgumentException]::new('Certificate 模式必须至少提供一个 -Files 路径。')
    }

    $resolvedFiles = [System.Collections.Generic.List[string]]::new()
    foreach ($file in @($Files)) {
        if ([string]::IsNullOrWhiteSpace($file)) {
            throw [System.ArgumentException]::new('-Files 含空路径。')
        }
        if (-not (Test-Path -LiteralPath $file -PathType Leaf)) {
            throw [System.ArgumentException]::new("待签名文件不存在：$file")
        }
        $ext = [System.IO.Path]::GetExtension($file).ToLowerInvariant()
        if ($ext -notin @('.exe', '.dll', '.msi')) {
            throw [System.ArgumentException]::new("Authenticode 只对 EXE/DLL/MSI 签名，拒绝 '$file'（ZIP 通过内部载荷验签，不套 Authenticode）。")
        }
        $resolvedFiles.Add((Resolve-Path -LiteralPath $file).Path)
    }

    foreach ($file in $resolvedFiles) {
        Write-Host "[sign] sign -> $file"
        & $signtool sign /sha1 $CertificateThumbprint /fd SHA256 /tr $TimestampUrl /td SHA256 /d 'ClipBridge' $file
        if ($LASTEXITCODE -ne 0) {
            throw [System.InvalidOperationException]::new("signtool sign 失败（exit $LASTEXITCODE）：$file")
        }
    }

    foreach ($file in $resolvedFiles) {
        $sig = Get-AuthenticodeSignature -LiteralPath $file
        if ($null -eq $sig.SignerCertificate) {
            throw [System.InvalidOperationException]::new("签名后没有 SignerCertificate：$file")
        }
        if ([string]$sig.Status -ne 'Valid') {
            throw [System.InvalidOperationException]::new("Authenticode 验证未通过（Status=$($sig.Status)）：$file")
        }
        if ([string]$sig.SignerCertificate.Thumbprint -ine $CertificateThumbprint) {
            throw [System.InvalidOperationException]::new("签名者 thumbprint 不匹配：$file")
        }
        if ($null -eq $sig.TimeStamperCertificate) {
            throw [System.InvalidOperationException]::new("签名缺少时间戳：$file")
        }

        & $signtool verify /pa /all /v $file
        if ($LASTEXITCODE -ne 0) {
            throw [System.InvalidOperationException]::new("signtool verify 失败（exit $LASTEXITCODE）：$file")
        }
        Write-Host "[sign] verified: $file"
    }
}
catch {
    Write-Error -ErrorAction Continue $_
    if ($_.Exception -is [System.ArgumentException] -or $_.Exception -is [System.UriFormatException]) {
        $exitCode = 2
    }
    else {
        $exitCode = 1
    }
}

exit $exitCode
