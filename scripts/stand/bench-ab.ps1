# Urfin stand: A/B bench of two builds (G-07), runs alternate A, B, A, B... on a quiet machine.
# Each exe runs `--bench [dir]` and prints a JSON line `{"bench":"ui",...,"raw":{...}}` (S9-05);
# its numbers and `raw` keys are compared. Builds before S9-05 print `bench ui=... key=value ...`.
# Prints the median of every numeric key per build and writes raw lines to -Out (JSON lines).
# Exes should be portable test copies (`urfin.portable` next to them), not the owner's install.
# Usage: powershell -File scripts\stand\bench-ab.ps1 -A <a\urfin.exe> -B <b\urfin.exe> [-Runs 4] [-Dir <path>] [-Out runs.jsonl]
param(
  [Parameter(Mandatory)][string]$A,
  [Parameter(Mandatory)][string]$B,
  [int]$Runs = 4,
  [string]$Dir = "",
  [string]$Out = ""
)

function Invoke-Bench([string]$exe) {
  $argv = @("--bench")
  if ($Dir) { $argv += $Dir }
  $out = & $exe @argv
  $kv = [ordered]@{}
  $json = $out | Where-Object { $_ -like '{"bench":*' } | Select-Object -Last 1
  if ($json) {
    $o = $json | ConvertFrom-Json
    $kv["ui"] = if ($o.raw -and $o.raw.ui) { $o.raw.ui } else { $o.bench }
    $props = @($o.PSObject.Properties)
    if ($o.raw) { $props += @($o.raw.PSObject.Properties) }
    foreach ($p in $props) {
      # numbers only: stable keys first, then raw k=v keys not already taken
      if ($p.Value -is [ValueType] -and $p.Value -isnot [bool] -and -not $kv.Contains($p.Name)) { $kv[$p.Name] = $p.Value }
    }
    return $kv
  }
  $line = $out | Where-Object { $_ -like "bench *" } | Select-Object -Last 1
  if (-not $line) { throw "no bench line from $exe" }
  foreach ($tok in ($line -split ' ' | Select-Object -Skip 1)) {
    $k, $v = $tok -split '=', 2
    $kv[$k] = $v
  }
  $kv
}

function Get-Median([double[]]$xs) {
  if (-not $xs) { return "-" }
  $s = $xs | Sort-Object
  $n = $s.Count
  if ($n % 2) { $s[[int][math]::Floor($n / 2)] } else { ($s[$n / 2 - 1] + $s[$n / 2]) / 2 }
}

$results = @{ A = @(); B = @() }
for ($i = 1; $i -le $Runs; $i++) {
  foreach ($side in "A", "B") {
    $exe = if ($side -eq "A") { $A } else { $B }
    Write-Host "run $i $side"
    $r = Invoke-Bench $exe
    $r["side"] = $side
    $r["run"] = $i
    $results[$side] += , $r
    if ($Out) { ($r | ConvertTo-Json -Compress) | Add-Content -LiteralPath $Out -Encoding utf8 }
  }
}

$keys = $results.A[0].Keys | Where-Object { $_ -notin "ui", "side", "run" }
"| key | A ($($results.A[0].ui)) | B ($($results.B[0].ui)) |"
"|---|---:|---:|"
foreach ($k in $keys) {
  $ma = Get-Median ($results.A | ForEach-Object { [double]$_[$k] })
  $mb = Get-Median ($results.B | Where-Object { $_.Contains($k) } | ForEach-Object { [double]$_[$k] })
  $fa = if ($ma -is [string]) { $ma } else { [math]::Round($ma, 2) }
  $fb = if ($mb -is [string]) { $mb } else { [math]::Round($mb, 2) }
  "| $k | $fa | $fb |"
}
