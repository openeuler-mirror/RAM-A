[CmdletBinding()]
param(
    [string]$HelperImage = "rama-binfmt-arm64:local",
    [string]$KeeperName = "rama-binfmt-keeper"
)

$ErrorActionPreference = "Stop"

$release = "deploy/v10.2.3-68"
$binfmtUrl = "https://github.com/tonistiigi/binfmt/releases/download/$release/binfmt_linux-amd64.tar.gz"
$qemuUrl = "https://github.com/tonistiigi/binfmt/releases/download/$release/qemu_v10.2.3_linux-amd64.tar.gz"
$binfmtSha256 = "9f955e980acb29986365766db7d36383a1dbbf2159f30b532a74474f6f5dd75d"
$qemuSha256 = "8e7d8f4c0c7809fc3fea0085199fd6b16f671e7c73d9bf6bec711e1cb535920a"
$workDir = Join-Path ([IO.Path]::GetTempPath()) "rama-binfmt-v10.2.3-68"
$binfmtArchive = Join-Path $workDir "binfmt.tar.gz"
$qemuArchive = Join-Path $workDir "qemu.tar.gz"

function Assert-LastExitCode([string]$Action) {
    if ($LASTEXITCODE -ne 0) {
        throw "$Action failed with exit code $LASTEXITCODE."
    }
}

function Get-VerifiedAsset([string]$Uri, [string]$Path, [string]$ExpectedHash) {
    if (-not (Test-Path -LiteralPath $Path) -or
        (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() -ne $ExpectedHash) {
        Invoke-WebRequest -Uri $Uri -OutFile $Path
    }
    $actualHash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -ne $ExpectedHash) {
        throw "SHA-256 mismatch for $Path. Expected $ExpectedHash, got $actualHash."
    }
}

New-Item -ItemType Directory -Force -Path $workDir | Out-Null
Get-VerifiedAsset -Uri $binfmtUrl -Path $binfmtArchive -ExpectedHash $binfmtSha256
Get-VerifiedAsset -Uri $qemuUrl -Path $qemuArchive -ExpectedHash $qemuSha256

& tar -xzf $binfmtArchive -C $workDir
Assert-LastExitCode "Extracting binfmt"
& tar -xzf $qemuArchive -C $workDir qemu-aarch64
Assert-LastExitCode "Extracting qemu-aarch64"

@'
FROM scratch
COPY binfmt /usr/bin/binfmt
COPY qemu-aarch64 /usr/bin/qemu-aarch64
ENTRYPOINT ["/usr/bin/binfmt"]
'@ | Set-Content -LiteralPath (Join-Path $workDir "Dockerfile") -Encoding utf8

& docker build --tag $HelperImage $workDir
Assert-LastExitCode "Building the local binfmt helper image"

$existingKeeper = & docker ps -a --filter "name=^/$KeeperName$" --format '{{.ID}}'
Assert-LastExitCode "Checking the binfmt keeper container"
if (-not [string]::IsNullOrWhiteSpace($existingKeeper)) {
    & docker rm --force $KeeperName | Out-Null
    Assert-LastExitCode "Replacing the binfmt keeper container"
}

& docker run --detach --platform linux/amd64 --name $KeeperName --privileged `
    openeuler/openeuler:24.03-lts-sp3 `
    /bin/sh -c 'mount -t binfmt_misc binfmt_misc /proc/sys/fs/binfmt_misc; exec sleep infinity'
Assert-LastExitCode "Starting the binfmt keeper container"

& docker run --privileged --rm -e QEMU_PRESERVE_ARGV0=1 $HelperImage `
    --uninstall qemu-aarch64 --install arm64
Assert-LastExitCode "Registering qemu-aarch64"

$machine = & docker run --rm --platform linux/arm64 --entrypoint /usr/bin/uname `
    openeuler/openeuler:24.03-lts-sp3 -m
Assert-LastExitCode "Running the ARM64 smoke test"
if ($machine.Trim() -ne "aarch64") {
    throw "ARM64 smoke test returned '$machine' instead of 'aarch64'."
}

Write-Host "ARM64 emulation is ready. Keep container '$KeeperName' running while building."
