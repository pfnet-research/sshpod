param(
    [string]$Version = "",
    [switch]$Yes,
    [string]$Prefix = ""
)

$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

function Get-Home {
    if (-not [string]::IsNullOrWhiteSpace($env:HOME)) {
        return $env:HOME
    }
    if (-not [string]::IsNullOrWhiteSpace($env:USERPROFILE)) {
        return $env:USERPROFILE
    }
    [Environment]::GetFolderPath("UserProfile")
}

function Resolve-Prefix([string]$Value) {
    if ([string]::IsNullOrWhiteSpace($Value)) {
        $Value = Join-Path (Get-Home) ".local\bin"
    }
    elseif ($Value.StartsWith("~")) {
        $Value = Join-Path (Get-Home) $Value.Substring(2)
    }
    [IO.Path]::GetFullPath($Value)
}

function Detect-Arch {
    switch ($env:PROCESSOR_ARCHITECTURE.ToLower()) {
        "amd64" { "amd64" }
        "arm64" { "arm64" }
        default { throw "Unsupported arch: $($env:PROCESSOR_ARCHITECTURE). Supported: amd64, arm64." }
    }
}

function Get-Version([string]$Value) {
    if (-not [string]::IsNullOrWhiteSpace($Value)) {
        return $Value
    }
    $headers = @{ "User-Agent" = "sshpod-install" }
    if ($env:GITHUB_TOKEN) {
        $headers["Authorization"] = "Bearer $($env:GITHUB_TOKEN)"
    }
    $apiUrl = "https://api.github.com/repos/pfnet-research/sshpod/releases/latest"
    $webUrl = "https://github.com/pfnet-research/sshpod/releases/latest"
    $version = ""

    function Try-Api {
        param($Headers, $Url)
        $resp = Invoke-WebRequest -UseBasicParsing -Headers $Headers -Uri $Url
        $json = $resp.Content | ConvertFrom-Json
        return $json.tag_name.TrimStart("v")
    }

    function Try-Redirect {
        param($Headers, $Url)
        $resp = Invoke-WebRequest -UseBasicParsing -Headers $Headers -Uri $Url -MaximumRedirection 5
        $uri = $resp.BaseResponse.ResponseUri.AbsoluteUri
        if (-not $uri) { return "" }
        return ($uri -replace '.*/tag/v?([^/]+)$','$1')
    }

    try {
        $version = Try-Api -Headers $headers -Url $apiUrl
    }
    catch {
        $version = ""
    }
    if ([string]::IsNullOrWhiteSpace($version)) {
        try {
            $version = Try-Redirect -Headers $headers -Url $webUrl
        }
        catch {
            $version = ""
        }
    }

    if ([string]::IsNullOrWhiteSpace($version)) {
        throw "Failed to determine latest version from GitHub releases."
    }
    return $version
}

# Releases up to 1.0.8 were published without SHA256SUMS; those are installed unverified.
function Confirm-Checksum([string]$BaseUrl, [string]$AssetName, [string]$AssetPath) {
    $sumsPath = "$AssetPath.SHA256SUMS"
    try {
        Invoke-WebRequest -UseBasicParsing -Headers @{ "User-Agent" = "sshpod-install" } -Uri "$BaseUrl/SHA256SUMS" -OutFile $sumsPath
    }
    catch {
        $status = 0
        if ($_.Exception.Response) {
            $status = [int]$_.Exception.Response.StatusCode
        }
        if ($status -eq 404) {
            Write-Warning "This release has no SHA256SUMS; skipping checksum verification."
            return
        }
        throw "Failed to download SHA256SUMS from '$BaseUrl'. Error: $($_.Exception.Message)"
    }

    $expected = ""
    foreach ($line in Get-Content $sumsPath) {
        $fields = $line.Trim() -split "\s+", 2
        if ($fields.Count -eq 2 -and $fields[1].TrimStart("*") -eq $AssetName) {
            $expected = $fields[0]
        }
    }
    if ([string]::IsNullOrWhiteSpace($expected)) {
        throw "SHA256SUMS does not list $AssetName."
    }

    $actual = (Get-FileHash -Algorithm SHA256 -Path $AssetPath).Hash
    if ($actual -ne $expected) {
        throw "Checksum mismatch for ${AssetName}: expected $expected, got $actual."
    }
    Write-Host "Verified checksum of $AssetName."
}

function Prompt-Configure([string]$ExePath) {
    if ($Yes) {
        & $ExePath configure
        return
    }
    $ans = Read-Host "Run sshpod configure to update ~/.ssh/config now? [y/N]"
    if ($ans -match "^[yY]$") {
        & $ExePath configure
    }
    else {
        Write-Host "Skipping ssh config update."
    }
}

function Main {
    $prefix = Resolve-Prefix $Prefix
    $version = Get-Version $Version
    $arch = Detect-Arch
    $assetName = "sshpod_${version}_windows_${arch}.zip"
    $baseUrl = "https://github.com/pfnet-research/sshpod/releases/download/v${version}"
    $url = "$baseUrl/$assetName"

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("sshpod-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Path $tmp -Force | Out-Null
    $binDir = Join-Path $tmp "bin"
    New-Item -ItemType Directory -Path $binDir -Force | Out-Null

    try {
        $assetPath = Join-Path $tmp $assetName
        Write-Host "Downloading $assetName ..."
        try {
            Invoke-WebRequest -UseBasicParsing -Headers @{ "User-Agent" = "sshpod-install" } -Uri $url -OutFile $assetPath
        }
        catch {
            throw "Failed to download release asset from '$url'. Error: $($_.Exception.Message)"
        }
        Confirm-Checksum $baseUrl $assetName $assetPath

        Expand-Archive -Path $assetPath -DestinationPath $binDir -Force
        $exe = Get-ChildItem -Path $binDir -Filter "sshpod.exe" -Recurse | Select-Object -First 1
        if (-not $exe) {
            throw "sshpod.exe not found in downloaded archive"
        }

        if (-not (Test-Path $prefix)) {
            New-Item -ItemType Directory -Path $prefix -Force | Out-Null
        }
        $dest = Join-Path $prefix "sshpod.exe"
        Copy-Item $exe.FullName $dest -Force
        Write-Host "Installed to $dest"

        if (-not (Get-Command "sshpod.exe" -ErrorAction SilentlyContinue)) {
            Write-Warning "Add $prefix to your PATH to run sshpod.exe"
        }

        Prompt-Configure $dest
    }
    finally {
        Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Main
