# Corpus RAR5 for crates/arc/tests/rar.rs (see README.md).
#
# Needs WinRAR's console Rar.exe (7.x: RAR5 only, RAR4 creation was removed in 7.0).
# Source files are deterministic: fixed content, fixed UTC mtime, fixed attributes.
# Archives are not byte-identical between runs (encryption salt), listings and data are.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File gen.ps1 [-Rar <path>] [-Tmp <dir>]

param(
    [string]$Rar = 'C:\Program Files\WinRAR\Rar.exe',
    [string]$Tmp = (Join-Path ([IO.Path]::GetTempPath()) 'urfin-rar-corpus')
)

$ErrorActionPreference = 'Stop'
$Out = $PSScriptRoot

# Non-ASCII names as escapes: the script stays ASCII.
function U([string]$s) { [regex]::Unescape($s) }

# Base time of the corpus; entry N gets Base + N minutes.
$Base = [datetime]::new(2024, 3, 1, 12, 0, 0, [DateTimeKind]::Utc)

# Pseudo-random bytes, same LCG as `lcg` in rar.rs.
function Lcg([long]$seed, [int]$len) {
    $b = New-Object byte[] $len
    $x = [long]$seed
    for ($i = 0; $i -lt $len; $i++) {
        $x = ($x * 1103515245 + 12345) -band 0xFFFFFFFFL
        $b[$i] = [byte](($x -shr 16) -band 0xFF)
    }
    $b
}

# Text: `count` lines "line <i> of <name>".
function Text([string]$name, [int]$count) {
    $sb = New-Object Text.StringBuilder
    for ($i = 0; $i -lt $count; $i++) { [void]$sb.Append("line $i of $name`n") }
    [Text.Encoding]::UTF8.GetBytes($sb.ToString())
}

# Fresh source tree `dir`: files = ordered list of @(relative path, bytes, minute).
function Tree([string]$dir, $files, $dirs) {
    if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }
    New-Item -ItemType Directory $dir | Out-Null
    foreach ($d in $dirs) { New-Item -ItemType Directory (Join-Path $dir $d[0]) -Force | Out-Null }
    foreach ($f in $files) {
        $p = Join-Path $dir $f[0]
        New-Item -ItemType Directory (Split-Path $p) -Force | Out-Null
        [IO.File]::WriteAllBytes($p, [byte[]]$f[1])
        $i = Get-Item -LiteralPath $p
        $i.Attributes = [IO.FileAttributes]::Normal
        $i.LastWriteTimeUtc = $Base.AddMinutes($f[2])
    }
    # Directory times last, children first (`dirs` lists parents first).
    for ($k = $dirs.Count - 1; $k -ge 0; $k--) {
        $d = $dirs[$k]
        $i = Get-Item -LiteralPath (Join-Path $dir $d[0])
        $i.Attributes = [IO.FileAttributes]::Directory
        $i.LastWriteTimeUtc = $Base.AddMinutes($d[1])
    }
}

# Rar.exe in `dir`: names relative, no config, no prompts.
function Pack([string]$dir, [string]$arc, [string[]]$sw) {
    Push-Location $dir
    try {
        $a = Join-Path $Out $arc
        if (Test-Path $a) { Remove-Item $a }
        & $Rar a -cfg- -idq -y -r -ma5 @sw $a '*'
        if ($LASTEXITCODE -ne 0) { throw "rar $arc exit $LASTEXITCODE" }
    } finally { Pop-Location }
}

New-Item -ItemType Directory $Tmp -Force | Out-Null

# r5-basic.rar: tree, empty file and directory, non-ASCII names, Windows attributes,
# BLAKE2 checksums, recovery record.
$privet = U '\u041f\u0440\u0438\u0432\u0435\u0442.txt'      # Privet.txt in Cyrillic
$katalog = U '\u043a\u0430\u0442\u0430\u043b\u043e\u0433'    # katalog
$fail = U '\u0444\u0430\u0439\u043b \u2116 1.txt'           # fail No 1.txt
$smile = U '\ud83d\ude00 \u65e5\u672c.txt'                   # emoji (surrogate pair) + CJK
$src = Join-Path $Tmp 'basic'
Tree $src @(
    @('readme.txt', (Text 'readme.txt' 40), 1),
    @('empty.txt', [byte[]]@(), 2),
    @('random.bin', (Lcg 1 3000), 3),
    @($privet, (Text $privet 3), 4),
    @("$katalog\$fail", (Text $fail 5), 5),
    @("$katalog\$smile", (Text $smile 7), 6),
    @('docs\a\deep.txt', (Text 'deep.txt' 9), 7),
    @('docs\ro.txt', (Text 'ro.txt' 2), 8),
    @('docs\.hidden', (Text '.hidden' 1), 9)
) @(@('docs', 20), @('docs\a', 21), @('void', 22), @($katalog, 23))
Set-ItemProperty -LiteralPath (Join-Path $src 'docs\ro.txt') -Name Attributes -Value ReadOnly
Set-ItemProperty -LiteralPath (Join-Path $src 'docs\.hidden') -Name Attributes -Value Hidden
Pack $src 'r5-basic.rar' @('-m3', '-htb', '-rr1%')

# r5-solid.rar: 50 similar text files in two directories, solid.
$files = for ($n = 0; $n -lt 50; $n++) {
    $name = 'f{0:d2}.txt' -f $n
    $sub = if ($n -lt 25) { 'a' } else { 'b' }
    , @("$sub\$name", (Text $name (10 + $n)), $n)
}
$src = Join-Path $Tmp 'solid'
Tree $src $files @(@('a', 60), @('b', 61))
Pack $src 'r5-solid.rar' @('-m5', '-s')

# r5-vol.partN.rar: 4 volumes of 16 KiB, a random file spans all of them.
$src = Join-Path $Tmp 'vol'
Tree $src @(
    @('1-before.txt', (Text '1-before.txt' 20), 1),
    @('2-big.bin', (Lcg 7 52000), 2),
    @('3-after.txt', (Text '3-after.txt' 30), 3)
) @()
Get-ChildItem $Out -Filter 'r5-vol.part*.rar' | Remove-Item
Pack $src 'r5-vol.rar' @('-m3', '-v16k')

# r5-pw.rar: data encrypted (-p), password "parol'" in Cyrillic; names are visible.
$src = Join-Path $Tmp 'pw'
Tree $src @(
    @('secret.txt', (Text 'secret.txt' 50), 1),
    @('open\empty.txt', [byte[]]@(), 2)
) (, @('open', 10))
Pack $src 'r5-pw.rar' @('-m3', ('-p' + (U '\u043f\u0430\u0440\u043e\u043b\u044c')))

# r5-hp.rar: headers encrypted (-hp), solid, password "secret".
$src = Join-Path $Tmp 'hp'
Tree $src @(
    @('x\one.txt', (Text 'one.txt' 30), 1),
    @('x\two.txt', (Text 'two.txt' 30), 2),
    @('three.bin', (Lcg 3 2000), 3)
) (, @('x', 10))
Pack $src 'r5-hp.rar' @('-m3', '-s', '-hpsecret')

Remove-Item -Recurse -Force $Tmp
Get-ChildItem $Out -Filter 'r5-*.rar' | Format-Table Name, Length
