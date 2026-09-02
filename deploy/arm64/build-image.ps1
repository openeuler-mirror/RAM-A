[CmdletBinding()]
param(
    [string]$ImageName = "xiaoo-rama:arm64-pr18",
    [string]$GlmToken = $env:GLM_CODING_TOKEN,
    [string]$OpenRouterKey = $env:OPENROUTER_API_KEY
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($GlmToken)) {
    throw "GLM Coding Plan token is required through -GlmToken or GLM_CODING_TOKEN."
}
if ([string]::IsNullOrWhiteSpace($OpenRouterKey)) {
    throw "OpenRouter key is required through -OpenRouterKey or OPENROUTER_API_KEY."
}

Write-Host "Building $ImageName for linux/arm64 from the latest AtomGit PR 18 head."
& docker buildx build `
    --platform linux/arm64 `
    --load `
    --build-arg "GLM_CODING_TOKEN=$GlmToken" `
    --build-arg "OPENROUTER_API_KEY=$OpenRouterKey" `
    --tag $ImageName `
    $PSScriptRoot

if ($LASTEXITCODE -ne 0) {
    throw "Docker build failed with exit code $LASTEXITCODE."
}

Write-Host "Built $ImageName successfully."
