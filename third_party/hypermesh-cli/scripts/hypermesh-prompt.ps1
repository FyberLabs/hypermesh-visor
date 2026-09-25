# Non-interactive Hypermesh chat for PowerShell.
# Stdout is only the model text. The process status matches the hypermesh binary.
# This script execs that binary. It does not open its own network client.
# Chat stays POST {chat base}/v1/chat/completions inside the binary.

[CmdletBinding()]
param(
    [Parameter(Position = 0, ValueFromRemainingArguments = $true)]
    [string[]] $Prompt,

    [string] $LeaseId,

    [string] $Model,

    [string] $System,

    [string] $Binary,

    [string] $ApiBase,

    [string] $ChatBase
)

$ErrorActionPreference = 'Continue'

function Write-Diag {
    param([string] $Message)
    [Console]::Error.WriteLine($Message)
}

function Get-HypermeshBinary {
    param([string] $Explicit)
    if ($Explicit) {
        if (-not (Test-Path -LiteralPath $Explicit)) {
            Write-Diag "hypermesh binary not found: $Explicit"
            exit 1
        }
        return (Resolve-Path -LiteralPath $Explicit).Path
    }
    foreach ($name in @('hypermesh', 'hm')) {
        $cmd = Get-Command $name -ErrorAction SilentlyContinue
        if ($null -ne $cmd -and $cmd.Source) {
            return $cmd.Source
        }
    }
    Write-Diag "hypermesh binary not found on PATH"
    exit 1
}

$bin = Get-HypermeshBinary -Explicit $Binary
$argList = New-Object System.Collections.Generic.List[string]
if ($ApiBase) {
    $argList.Add('--api-base')
    $argList.Add($ApiBase)
}
if ($ChatBase) {
    $argList.Add('--chat-base')
    $argList.Add($ChatBase)
}
$argList.Add('prompt')
$argList.Add('--script')
if ($LeaseId) {
    $argList.Add('--lease-id')
    $argList.Add($LeaseId)
}
if ($Model) {
    $argList.Add('--model')
    $argList.Add($Model)
}
if ($System) {
    $argList.Add('--system')
    $argList.Add($System)
}
if ($Prompt) {
    foreach ($part in $Prompt) {
        $argList.Add($part)
    }
}

# Do not capture streams. Stdout of the binary is the model text.
& $bin @($argList.ToArray())
$code = $LASTEXITCODE
if ($null -eq $code) {
    $code = 1
}
exit $code
