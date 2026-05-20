$ErrorActionPreference = "Stop"

$WeightsDir = Join-Path (Split-Path -Parent $PSScriptRoot) "weights"
New-Item -ItemType Directory -Force -Path $WeightsDir | Out-Null

$Items = @(
  @{
    Url  = "https://huggingface.co/nomic-ai/nomic-embed-text-v1.5-GGUF/resolve/main/nomic-embed-text-v1.5.Q4_K_M.gguf"
    Path = Join-Path $WeightsDir "nomic-q4.gguf"
    Sha  = "d4e388894e09cf3816e8b0896d81d265b55e7a9fff9ab03fe8bf4ef5e11295ac"
  },
  @{
    Url  = "https://huggingface.co/nomic-ai/nomic-embed-text-v1.5/resolve/main/tokenizer.json"
    Path = Join-Path $WeightsDir "tokenizer.json"
    Sha  = "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66"
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
