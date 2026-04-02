$ErrorActionPreference = "Stop"
$msvcBin = "C:\Users\user\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin"
$vsBin = "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.44.35207\bin\Hostx64\x64"
$env:PATH = "$msvcBin;$vsBin;$env:PATH"
cargo build --release @args
