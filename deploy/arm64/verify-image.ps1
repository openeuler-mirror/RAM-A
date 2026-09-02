[CmdletBinding()]
param(
    [string]$ImageName = "xiaoo-rama:arm64-pr18"
)

$ErrorActionPreference = "Stop"

$architecture = & docker image inspect $ImageName --format "{{.Architecture}}"
if ($LASTEXITCODE -ne 0) {
    throw "Unable to inspect image $ImageName."
}
if ($architecture.Trim() -ne "arm64") {
    throw "Expected arm64 image, got '$($architecture.Trim())'."
}

& docker run --rm --platform linux/arm64 `
    --entrypoint /opt/ram-a/scripts/image-info.sh `
    $ImageName
if ($LASTEXITCODE -ne 0) {
    throw "Image metadata verification failed."
}

foreach ($volume in @("ram-a-verify-data", "xiaoo-verify-data")) {
    & docker volume inspect $volume *> $null
    if ($LASTEXITCODE -ne 0) {
        & docker volume create $volume | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw "Failed to create Docker volume $volume."
        }
    }
}

$verifyOutput = & docker run --rm --platform linux/arm64 `
    -v ram-a-verify-data:/var/lib/ram-a `
    -v xiaoo-verify-data:/var/lib/xiaoo `
    $ImageName verify 2>&1
$verifyExitCode = $LASTEXITCODE
$verifyOutput | ForEach-Object { Write-Output $_ }

if ($verifyExitCode -ne 0) {
    throw "RAM-A ingestion verification failed with exit code $verifyExitCode."
}
if (($verifyOutput -join "`n") -notmatch "RAM_A_INGEST_OK") {
    throw "Verification exited successfully but did not emit RAM_A_INGEST_OK."
}

Write-Host "Verified ${ImageName}: arm64 image and RAM-A ingestion pipeline passed."
