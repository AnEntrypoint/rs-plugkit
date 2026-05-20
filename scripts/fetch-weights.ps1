$ErrorActionPreference = "Stop"

$WeightsDir = Join-Path (Split-Path -Parent $PSScriptRoot) "weights"
New-Item -ItemType Directory -Force -Path $WeightsDir | Out-Null

$Items = @(
  @{
    Url  = "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/model.safetensors"
    Path = Join-Path $WeightsDir "minilm-l6-v2.safetensors"
    Sha  = "53aa51172d142c89d9012cce15ae4d6cc0ca6895895114379cacb4fab128d9db"
  },
  @{
    Url  = "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json"
    Path = Join-Path $WeightsDir "tokenizer.json"
    Sha  = "be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037"
  }
)

function Test-Sha {
  param([string]$Path, [string]$Expected)
  if (-not (Test-Path $Path)) { return $false }
  $actual = (Get-FileHash -Algorithm SHA256 -Path $Path).Hash.ToLower()
  return $actual -eq $Expected.ToLower()
}

foreach ($it in $Items) {
  if (Test-Sha -Path $it.Path -Expected $it.Sha) {
    Write-Output "ok: $($it.Path) (sha matches)"
    continue
  }
  Write-Output "fetching $($it.Url) -> $($it.Path)"
  Invoke-WebRequest -Uri $it.Url -OutFile $it.Path -UseBasicParsing
  if (-not (Test-Sha -Path $it.Path -Expected $it.Sha)) {
    Write-Error "sha mismatch for $($it.Path)"
    exit 1
  }
}

Write-Output "weights ready in $WeightsDir"
