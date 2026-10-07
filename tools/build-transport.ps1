<#
.SYNOPSIS
  Собирает MCP-транспорт ИИ-оператора (addin/mcp-transport) под Windows x64/x86 и кладёт zip
  с манифестом в общий макет ядра ИИО_ТранспортMCP.

.DESCRIPTION
  Сборка воспроизводимая: тот же rustc (1.98.1), Cargo.lock (--locked), пути исходников заменены
  (--remap-path-prefix), без метки времени в PE-заголовке, zip с фиксированными датами и порядком.
  Тулчейны GNU и dlltool из WinLibs — как для прежней компоненты. Хеши DLL и zip
  пишутся в addin/mcp-transport/SHA256SUMS.

  -Verify собирает дважды в разные каталоги target и сравнивает хеши: макет в git должен получаться
  из исходников бит в бит. Макет при этом не меняется.

  Макет коммитится вместе с исходниками: ядро собирается и устанавливается без Rust.

.EXAMPLE
  ./tools/build-transport.ps1
  ./tools/build-transport.ps1 -Verify
#>
param(
	[switch]$Verify
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$crate = Join-Path $repo 'addin\mcp-transport'
$templateDir = Join-Path $repo 'src\ai-operator\CommonTemplates\ИИО_ТранспортMCP\Ext'
$rustcVersion = '1.98.1'

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
	$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
}
# Линкует self-contained mingw из тулчейна Rust: 64-битный gcc из WinLibs в PATH сломал бы сборку x86.
$env:Path = ($env:Path -split ';' | Where-Object { $_ -and -not (Test-Path (Join-Path $_ 'gcc.exe')) }) -join ';'
if (-not (Get-Command dlltool -ErrorAction SilentlyContinue)) {
	$winlibs = Get-ChildItem "$env:LOCALAPPDATA\Microsoft\WinGet\Packages" -Directory -Filter 'BrechtSanders.WinLibs*' -ErrorAction SilentlyContinue |
		ForEach-Object { Join-Path $_.FullName 'mingw64\x86_64-w64-mingw32\bin' } | Where-Object { Test-Path (Join-Path $_ 'dlltool.exe') } |
		Select-Object -First 1
	if (-not $winlibs) { throw 'Не найден dlltool.exe: winget install BrechtSanders.WinLibs.POSIX.MSVCRT' }
	$env:Path += ";$winlibs"
}

$targets = @(
	@{ Arch = 'x64'; Triple = 'x86_64-pc-windows-gnu'; Toolchain = 'stable-x86_64-pc-windows-gnu'; Manifest = 'x86_64' },
	@{ Arch = 'x32'; Triple = 'i686-pc-windows-gnu'; Toolchain = 'stable-i686-pc-windows-gnu'; Manifest = 'i386' }
)
foreach ($target in $targets) {
	$version = (& rustup run $target.Toolchain rustc --version) -split ' ' | Select-Object -Index 1
	if ($version -ne $rustcVersion) {
		throw "Тулчейн $($target.Toolchain): rustc $version, нужен $rustcVersion — другая версия даёт другие байты DLL"
	}
}

# Лицензии зависимостей: компонента выходит под AGPL-3.0, допустимы только совместимые разрешительные лицензии.
# Выражение SPDX вычисляется: в «A OR B» достаточно одной разрешённой ветки, в «A AND B» нужны обе.
# Так ловится и новая зависимость, и смена лицензии при обновлении крейта.
$allowedLicenses = 'MIT', 'Apache-2.0', 'BSD-2-Clause', 'BSD-3-Clause', 'ISC', 'Zlib', 'Unicode-3.0', 'Unlicense'
function Test-License([string]$Expression) {
	if (-not $Expression) { return $false }
	$tokens = ($Expression -replace '\s+WITH\s+\S+', '' -replace '([()])', ' $1 ') -split '\s+' | Where-Object { $_ }
	$code = ($tokens | ForEach-Object {
			switch ($_) {
				'OR' { '-or' } 'AND' { '-and' } '(' { '(' } ')' { ')' }
				default { if ($_ -in $allowedLicenses) { '$true' } else { '$false' } }
			}
		}) -join ' '
	return [bool](Invoke-Expression $code)
}
$used = @{}
$packages = @{}
foreach ($target in $targets) {
	$metadata = & cargo metadata --format-version 1 --locked --filter-platform $target.Triple `
		--manifest-path (Join-Path $crate 'Cargo.toml') | ConvertFrom-Json
	if ($LASTEXITCODE -ne 0) { throw 'cargo metadata не удался' }
	$metadata.resolve.nodes | ForEach-Object { $used[$_.id] = $true }
	$metadata.packages | ForEach-Object { $packages[$_.id] = $_ }
}
$badLicenses = $packages.Values | Where-Object { $used[$_.id] -and $_.name -ne 'ai-operator-mcp-transport' } |
	Where-Object { -not (Test-License $_.license) } | ForEach-Object { "$($_.name) $($_.version): $($_.license)" }
if ($badLicenses) { throw "Зависимости с лицензией вне списка: $($badLicenses -join '; ')" }

$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$rustupHome = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE '.rustup' }
# Поля CARGO_ENCODED_RUSTFLAGS разделяет символ 0x1F. Флаги одинаковы для обеих архитектур.
$env:CARGO_ENCODED_RUSTFLAGS = @(
	"--remap-path-prefix=$crate=/src",
	"--remap-path-prefix=$cargoHome=/cargo",
	"--remap-path-prefix=$rustupHome=/rustup",
	'-Clink-arg=-Wl,--no-insert-timestamp'
) -join [char]0x1F

$packageVersion = (Select-String -Path (Join-Path $crate 'Cargo.toml') -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value

function Build-Package([string]$TargetDir, [string]$Out) {
	Push-Location $crate
	try {
		foreach ($target in $targets) {
			Write-Host ">> cargo build $($target.Triple)"
			& cargo "+$($target.Toolchain)" build --release --locked --lib --target $target.Triple --target-dir $TargetDir
			if ($LASTEXITCODE -ne 0) { throw "Сборка $($target.Arch) не удалась" }
		}
	} finally {
		Pop-Location
	}

	if (Test-Path $Out) { [IO.Directory]::Delete($Out, $true) }
	New-Item -ItemType Directory -Force $Out | Out-Null
	$components = @()
	foreach ($target in $targets) {
		$built = Join-Path $TargetDir "$($target.Triple)\release\ai_operator_mcp.dll"
		# Платформа кеширует компоненты в %APPDATA%\1C\1cv8\ExtCompT по имени файла из манифеста:
		# с прежним именем клиент молча грузил бы старую DLL, поэтому в имя входит хеш содержимого.
		$hash = (Get-FileHash $built -Algorithm SHA256).Hash.Substring(0, 8).ToLower()
		$name = "AiOperatorMcp_$($target.Arch)-$packageVersion-$hash.dll"
		Copy-Item $built (Join-Path $Out $name)
		$components += "`t<component os=`"Windows`" path=`"$name`" type=`"native`" arch=`"$($target.Manifest)`" />"
	}
	$manifest = @(
		'<?xml version="1.0" encoding="UTF-8"?>'
		"<bundle xmlns=`"http://v8.1c.ru/8.2/addin/bundle`" name=`"AiOperatorMcp`" version=`"$packageVersion`" description=`"MCP-транспорт ИИ-оператора (AGPL-3.0)`">"
		$components
		'</bundle>'
	) -join "`r`n"
	[IO.File]::WriteAllText((Join-Path $Out 'Manifest.xml'), $manifest + "`r`n", (New-Object Text.UTF8Encoding $false))

	$zip = Join-Path $Out 'AiOperatorMcp.zip'
	Add-Type -AssemblyName System.IO.Compression
	$stream = [IO.File]::Open($zip, [IO.FileMode]::CreateNew)
	try {
		$archive = New-Object IO.Compression.ZipArchive($stream, [IO.Compression.ZipArchiveMode]::Create)
		try {
			# Фиксированные порядок и дата записей: zip зависит только от содержимого.
			$files = @('Manifest.xml') + (Get-ChildItem $Out -Filter '*.dll' | Sort-Object Name | ForEach-Object Name)
			foreach ($file in $files) {
				$entry = $archive.CreateEntry($file, [IO.Compression.CompressionLevel]::Optimal)
				$entry.LastWriteTime = [DateTimeOffset]::new(1980, 1, 1, 0, 0, 0, [TimeSpan]::Zero)
				$writer = $entry.Open()
				try {
					$bytes = [IO.File]::ReadAllBytes((Join-Path $Out $file))
					$writer.Write($bytes, 0, $bytes.Length)
				} finally {
					$writer.Dispose()
				}
			}
		} finally {
			$archive.Dispose()
		}
	} finally {
		$stream.Dispose()
	}
	return $zip
}

function Get-Hashes([string]$Dir) {
	Get-ChildItem $Dir -File | Where-Object { $_.Extension -in '.dll', '.zip' } | Sort-Object Name |
		ForEach-Object { "$((Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower())  $($_.Name)" }
}

$buildRoot = Join-Path $repo 'build\transport'
if ($Verify) {
	# Обе сборки с нуля: кеш прошлой сборки не должен подменять проверку.
	foreach ($dir in 'target-a', 'target-b') {
		$path = Join-Path $buildRoot $dir
		if (Test-Path $path) { [IO.Directory]::Delete($path, $true) }
	}
	$first = Get-Hashes (Split-Path (Build-Package (Join-Path $buildRoot 'target-a') (Join-Path $buildRoot 'out-a')))
	$second = Get-Hashes (Split-Path (Build-Package (Join-Path $buildRoot 'target-b') (Join-Path $buildRoot 'out-b')))
	$sums = Get-Content (Join-Path $crate 'SHA256SUMS') -ErrorAction SilentlyContinue
	$first | ForEach-Object { Write-Host $_ }
	if (Compare-Object $first $second) { throw 'Две сборки дали разные байты: сборка не воспроизводима' }
	if ($sums -and (Compare-Object $first $sums)) { throw 'Сборка не совпадает с SHA256SUMS: макет в git собран из других исходников' }
	Write-Host 'Сборка воспроизводима и совпадает с SHA256SUMS'
	return
}

$zip = Build-Package (Join-Path $buildRoot 'target') (Join-Path $buildRoot 'out')
New-Item -ItemType Directory -Force $templateDir | Out-Null
Copy-Item $zip (Join-Path $templateDir 'Template.bin') -Force
$hashes = Get-Hashes (Split-Path $zip)
[IO.File]::WriteAllLines((Join-Path $crate 'SHA256SUMS'), [string[]]$hashes, (New-Object Text.UTF8Encoding $false))
$hashes | ForEach-Object { Write-Host $_ }
Write-Host "Макет: $templateDir\Template.bin"
