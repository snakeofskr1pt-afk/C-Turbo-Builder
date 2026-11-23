<#
.SYNOPSIS
    Publish cpp-turbo-builder to GitHub: add remote, push main, create and push a tag.

.EXAMPLE
    .\scripts\publish.ps1 -RepoUrl https://github.com/myaccount/cpp-turbo-builder.git

.EXAMPLE
    # see what would happen without touching the network
    .\scripts\publish.ps1 -RepoUrl https://github.com/myaccount/cpp-turbo-builder.git -DryRun

.NOTES
    Authentication: Windows ships with Git Credential Manager (helper "manager"),
    so `git push` usually opens a browser window where you sign in.
    Without it, use a Personal Access Token as the password (see docs/GITHUB.md).
#>
[CmdletBinding()]
param(
    [string]$RepoUrl = "",
    [string]$Tag = "v0.1.0",
    [switch]$SkipTag,
    [switch]$DryRun,
    [switch]$Force
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
Write-Host "repository: $repo"

function Invoke-Git {
    param([string[]]$Arguments)
    $display = "git " + ($Arguments -join " ")
    if ($DryRun) {
        Write-Host "[dry-run] $display"
        return
    }
    $previous = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    & git @Arguments
    $code = $LASTEXITCODE
    $ErrorActionPreference = $previous
    if ($code -ne 0) {
        throw "FAILED ($code): $display"
    }
}

function Get-RemoteHeads {
    # Returns the remote branches; an empty result means "empty repository".
    $previous = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $lines = & git ls-remote --heads origin 2>$null
    $code = $LASTEXITCODE
    $ErrorActionPreference = $previous
    if ($code -ne 0) {
        throw "Cannot reach origin ($RepoUrl). Check the network, the URL and your credentials (git ls-remote exited with $code)."
    }
    return @($lines | Where-Object { $_ -and $_.Trim() })
}

Push-Location $repo
try {
    if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
        throw "git is not installed. Install Git for Windows first: https://git-scm.com/download/win"
    }

    & git rev-parse --is-inside-work-tree *> $null
    if ($LASTEXITCODE -ne 0) {
        throw "`"$repo`" is not a git repository."
    }

    $commits = (& git rev-list --count HEAD 2>$null)
    if (-not $commits -or [int]$commits -lt 1) {
        throw "There are no commits to publish yet."
    }
    Write-Host "commits  : $commits"

    $name = (& git config user.name)
    $email = (& git config user.email)
    if (-not $name -or -not $email) {
        throw "Git identity is not set. Run:`n  git config --global user.name `"Your Name`"`n  git config --global user.email `"you@example.com`""
    }
    Write-Host "author   : $name <$email>"

    $branch = (& git branch --show-current).Trim()
    if (-not $branch) { $branch = "main" }
    Write-Host "branch   : $branch"

    $dirty = (& git status --porcelain)
    if ($dirty) {
        Write-Warning "Working tree has uncommitted changes. They will NOT be pushed until you commit them."
    }

    if (-not $RepoUrl) {
        if ($DryRun) {
            $RepoUrl = "https://github.com/USERNAME/cpp-turbo-builder.git"
        } else {
            throw "Pass the repository URL, e.g. -RepoUrl https://github.com/myaccount/cpp-turbo-builder.git"
        }
    }
    if ($RepoUrl -notmatch "^(https://|git@)") {
        throw "-RepoUrl should look like https://github.com/<account>/<repo>.git"
    }
    Write-Host "remote   : $RepoUrl"

    $remotes = @(& git remote 2>$null)
    if ($remotes -contains "origin") {
        Invoke-Git @("remote", "set-url", "origin", $RepoUrl)
    } else {
        Invoke-Git @("remote", "add", "origin", $RepoUrl)
    }

    Write-Host ""
    if (-not $DryRun) {
        $remoteHeads = Get-RemoteHeads
        if ($remoteHeads.Count -gt 0 -and -not $Force) {
            throw @"
The GitHub repository is NOT empty - it already has commits (usually a README
or .gitignore created through the web UI). A plain push would be rejected.

Options:
  A) Easiest: delete that repository on GitHub and create a new EMPTY one
     (no README, no .gitignore, no license), then rerun this script.
  B) Keep the remote history and merge it manually:
       git pull --rebase origin $branch --allow-unrelated-histories
       git push -u origin $branch
     then rerun this script to publish the tag ($Tag).
  C) Overwrite the remote branch (destroys those remote commits):
       rerun this script with -Force
"@
        }
    }

    Write-Host "Pushing $branch ..."
    if ($Force) {
        Invoke-Git @("push", "--force-with-lease", "-u", "origin", $branch)
    } else {
        Invoke-Git @("push", "-u", "origin", $branch)
    }

    if (-not $SkipTag) {
        $tags = @(& git tag)
        if ($tags -notcontains $Tag) {
            Invoke-Git @("tag", "-a", $Tag, "-m", "cpp-turbo-builder $Tag")
        }
        Write-Host "Pushing tag $Tag (this triggers the Release workflow) ..."
        Invoke-Git @("push", "origin", $Tag)
    }

    Write-Host ""
    Write-Host "Done."
    Write-Host "  repository: $($RepoUrl -replace '\.git$','')"
    if (-not $SkipTag) {
        Write-Host "  releases  : $($RepoUrl -replace '\.git$','')/releases"
    }
}
finally {
    Pop-Location
}
