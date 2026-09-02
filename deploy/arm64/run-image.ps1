[CmdletBinding()]
param(
    [string]$ImageName = "xiaoo-rama:arm64-pr18",
    [string]$ContainerName = "xiaoo-rama",
    [switch]$Verify
)

$ErrorActionPreference = "Stop"

foreach ($volume in @("ram-a-data", "xiaoo-data")) {
    & docker volume inspect $volume *> $null
    if ($LASTEXITCODE -ne 0) {
        & docker volume create $volume | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw "Failed to create Docker volume $volume."
        }
    }
}

$arguments = @(
    "run", "--rm", "-it",
    "--platform", "linux/arm64",
    "--name", $ContainerName,
    "-p", "18081:18081",
    "-v", "ram-a-data:/var/lib/ram-a",
    "-v", "xiaoo-data:/var/lib/xiaoo",
    $ImageName
)
if ($Verify) {
    $arguments += "verify"
}

& docker @arguments
if ($LASTEXITCODE -ne 0) {
    throw "Container exited with code $LASTEXITCODE."
}
