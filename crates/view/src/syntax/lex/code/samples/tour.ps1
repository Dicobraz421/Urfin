#Requires -Version 5.1
<#
.SYNOPSIS
    Обзор языка: границы на каждом кандидате, окно = полный разбор.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Path,
    [int]$Count = 3
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-Thing {
    [CmdletBinding()]
    param([string]$Name, [switch]$Force)

    begin {
        $list = @()
    }
    process {
        $list += $Name
        Write-Verbose "added $Name at $(Get-Date -Format 'o')"
    }
    end {
        return $list
    }
}

function Test-Value($a, $b = 2)
{
    if ($a -gt $b) {
        "больше"
    }
    elseif ($a -eq $b) {
        'равно'
    }
    else {
        "меньше: $($a + $b) и `"кавычка`""
    }
}

$hash = @{
    Name  = 'x'
    Value = 1
    Inner = @{ A = 1; B = 2 }
}

$text = @"
строка $hash
"не конец"
$(Get-Thing -Name 'a')
"@

$raw = @'
без подстановок $x
'@

foreach ($i in 1..$Count) {
    switch ($i) {
        1 { 'один' }
        2 {
            'два'
            break
        }
        default { "прочее $i" }
    }
}

for ($i = 0; $i -lt 3; $i++) {
    $i * 2 | ForEach-Object { $_ + 1 } |
        Where-Object { $_ -gt 1 }
}

while ($true) {
    break
}

do {
    $n = Read-Host
} while ($n -ne 'q')

do {
    $n--
}
until ($n -lt 0)

try {
    Get-Item $Path -ErrorAction Stop
}
catch [System.IO.IOException] {
    Write-Error $_
}
finally {
    Write-Host 'done'
}

class Point {
    [int]$X
    [int]$Y

    Point([int]$x, [int]$y) {
        $this.X = $x
        $this.Y = $y
    }

    [string] ToString() {
        return "($($this.X), $($this.Y))"
    }
}

enum Color {
    Red
    Green = 3
}

$p = [Point]::new(1, 2)
$s = "a" +
    "b"
$long = Get-ChildItem -Path . `
    -Recurse
$script = {
    param($x)
    $x + 1
}

Invoke-Command -ScriptBlock {
    Write-Host 'внутри'
    Write-Host 'блока'
} -ArgumentList 1

$items | Sort-Object Name, Value | Select-Object -First 1
& $script 1
& "C:\Program Files\app.exe" /c 'x y' --flag=1
.\local.ps1 -Name a
. $PSScriptRoot\common.ps1
git log --oneline -- path
cmd /c echo it's #не комментарий ли
${braced var} = 1
$env:PATH += ';C:\x'
$a[0] = $b.Items[1].Name
[int]$typed = '5'
$v = if ($a) { 1 } else { 2 }
exit 0
