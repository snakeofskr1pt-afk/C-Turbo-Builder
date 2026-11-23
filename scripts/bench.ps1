<#
.SYNOPSIS
    Measure cold (empty cache) vs hot (fully cached) build times for a C++
    project — the "Day 4" measurement from the original plan.

.DESCRIPTION
    Requires a release build of turbo and a real compiler on PATH (g++/clang++/cl).
    Cold runs wipe the cache and .build directory first; hot runs reuse the cache.

.EXAMPLE
    .\scripts\bench.ps1 -Root C:\work\mygame -Compiler g++ -ExtraFlags "-Iinclude -O2" -Jobs 16 -ChunkSize 10
#>
[CmdletBinding()]
param(
    [string]$Root = ".",
    [string]$Compiler = "g++",
    [string]$ExtraFlags = "",
    [int]$Jobs = 0,
    [int]$ChunkSize = 8,
    [int]$Runs = 3,
    [string]$Out = "",
    [string]$CacheDir = ""
)

$ErrorActionPreference = "Stop"

$repo = Split-Path -Parent $PSScriptRoot
$turbo = Join-Path $repo "target\release\turbo.exe"
if (-not (Test-Path $turbo)) {
    throw "turbo is not built yet. Run: cargo build --release"
}

$Root = (Resolve-Path $Root).Path
if ($Jobs -le 0) {
    $Jobs = [Environment]::ProcessorCount
}
if (-not $CacheDir) {
    $CacheDir = Join-Path $env:TEMP "turbo-bench-cache"
}
$output = if ($Out) { $Out } else { Join-Path $Root "app.exe" }

$compilerFlags = @()
if ($ExtraFlags.Trim()) {
    $compilerFlags = $ExtraFlags.Trim() -split '\s+'
}

$buildDir = Join-Path $Root ".build"

function Invoke-Build {
    $stopwatch = [Diagnostics.Stopwatch]::StartNew()
    & $turbo build --root $Root --jobs $Jobs --chunk-size $ChunkSize `
        --out $output --cache-dir $CacheDir --quiet `
        -- $Compiler @compilerFlags 2>$null
    $exitCode = $LASTEXITCODE
    $stopwatch.Stop()
    if ($exitCode -ne 0) {
        throw "turbo build failed with exit code $exitCode"
    }
    [math]::Round($stopwatch.Elapsed.TotalMilliseconds, 1)
}

Write-Host "project : $Root"
Write-Host "compiler: $Compiler $ExtraFlags"
Write-Host "jobs    : $Jobs, chunk size: $ChunkSize, runs: $Runs"
Write-Host "cache   : $CacheDir"
Write-Host ""

$cold = @()
for ($i = 1; $i -le $Runs; $i++) {
    if (Test-Path $CacheDir) { Remove-Item $CacheDir -Recurse -Force }
    if (Test-Path $buildDir) { Remove-Item $buildDir -Recurse -Force }
    $ms = Invoke-Build
    Write-Host ("cold #{0}: {1,8:N1} ms" -f $i, $ms)
    $cold += $ms
}

$hot = @()
for ($i = 1; $i -le $Runs; $i++) {
    $ms = Invoke-Build
    Write-Host ("hot  #{0}: {1,8:N1} ms" -f $i, $ms)
    $hot += $ms
}

$coldAvg = ($cold | Measure-Object -Average).Average
$hotAvg = ($hot | Measure-Object -Average).Average
$speedup = if ($hotAvg -gt 0) { $coldAvg / $hotAvg } else { 0 }

Write-Host ""
Write-Host ("cold avg: {0,8:N1} ms" -f $coldAvg)
Write-Host ("hot  avg: {0,8:N1} ms" -f $hotAvg)
Write-Host ("speedup : {0,8:N2}x" -f $speedup)
