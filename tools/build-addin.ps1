<#
.SYNOPSIS
  Собирает внешнюю компоненту WebTransport с проверкой Bearer-токена под Windows x64/x86
  и упаковывает её в zip — макет Мсп_webTransport для client_mcp.

.DESCRIPTION
  Исходники — форк web-transport-addin с проверкой заголовка Authorization: Bearer. Скрипт скачивает
  закреплённый коммит форка в vendor/web-transport-addin, а если после обновления репозитория коммит
  сменился, переключает каталог на новый. Каталог, указанный в -Source, собирается как есть.

  С -IfChanged сборка пропускается, если компонента уже собрана из закреплённого коммита.

  Сборка GNU-таргетами, как в CI исходного проекта. Нужны:
    - rustup с тулчейнами stable-x86_64-pc-windows-gnu и stable-i686-pc-windows-gnu;
    - dlltool.exe в PATH (binutils из WinLibs: winget install BrechtSanders.WinLibs.POSIX.MSVCRT,
      в PATH — только каталог ...\mingw64\x86_64-w64-mingw32\bin, без gcc).
  В zip попадают только Windows-библиотеки: на Linux и macOS клиент такую компоненту не подключит,
  а не запустит сервер без проверки токена.

.EXAMPLE
  ./tools/build-addin.ps1
#>
param(
	[string]$Source = (Join-Path (Split-Path -Parent $PSScriptRoot) 'vendor\web-transport-addin'),
	[string]$Out = (Join-Path (Split-Path -Parent $PSScriptRoot) 'build\addin'),
	[switch]$IfChanged
)

$ErrorActionPreference = 'Stop'
$forkUrl = 'https://github.com/ui99ru/web-transport-addin.git'
$forkCommit = '6487f51596bd010c2a0d5c0ee9522befa9ba3c0c'

$buildInfo = Join-Path $Out 'BUILD.txt'
if ($IfChanged -and (Test-Path (Join-Path $Out 'WebTransportAddIn.zip')) -and (Test-Path $buildInfo)) {
	$built = (Get-Content $buildInfo | Where-Object { $_ -like 'commit=*' }) -replace '^commit=', ''
	if ($built -and $built -notlike '*-dirty' -and $forkCommit.StartsWith($built)) {
		Write-Host "Компонента уже собрана из коммита $built"
		return
	}
}

if (-not $PSBoundParameters.ContainsKey('Source')) {
	if (-not (Test-Path $Source)) { git init --quiet $Source }
	$head = git -C $Source rev-parse --quiet --verify HEAD
	if ($head -ne $forkCommit) {
		if (git -C $Source status --porcelain --untracked-files=no) {
			throw "В $Source есть изменения, а сборке нужен коммит $forkCommit. Сохраните изменения в другом месте или удалите каталог"
		}
		# Только закрепленный коммит, без истории: полный клон форка весит около 100 МБ.
		Write-Host ">> git fetch $forkUrl $forkCommit"
		git -C $Source fetch --quiet --depth 1 $forkUrl $forkCommit
		if ($LASTEXITCODE -ne 0) { throw "Не удалось получить коммит $forkCommit из $forkUrl" }
		git -C $Source checkout --quiet FETCH_HEAD
		if ($LASTEXITCODE -ne 0) { throw "Не удалось переключиться на коммит $forkCommit" }
	}
} elseif (-not (Test-Path $Source)) {
	throw "Не найден каталог исходников компоненты: $Source"
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
	$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
	throw 'Не найден cargo. Установите Rust (rustup) и тулчейны stable-x86_64-pc-windows-gnu и stable-i686-pc-windows-gnu'
}
# Линкует self-contained mingw из тулчейна Rust: 64-битный gcc в PATH сломал бы сборку x86.
$env:Path = ($env:Path -split ';' | Where-Object { $_ -and -not (Test-Path (Join-Path $_ 'gcc.exe')) }) -join ';'
if (-not (Get-Command dlltool -ErrorAction SilentlyContinue)) {
	throw 'Не найден dlltool.exe: добавьте в PATH каталог binutils из WinLibs (...\mingw64\x86_64-w64-mingw32\bin)'
}

Push-Location $Source
try {
	Write-Host '>> cargo build x86_64-pc-windows-gnu'
	cargo +stable-x86_64-pc-windows-gnu build --release --target x86_64-pc-windows-gnu
	if ($LASTEXITCODE -ne 0) { throw 'Сборка x64 не удалась' }

	Write-Host '>> cargo build i686-pc-windows-gnu'
	cargo +stable-i686-pc-windows-gnu build --release --target i686-pc-windows-gnu
	if ($LASTEXITCODE -ne 0) { throw 'Сборка x86 не удалась' }

	$version = ((cargo pkgid) -split '[@#]')[-1]
	$commit = git rev-parse --short HEAD
	$dirty = if (git status --porcelain -- src Cargo.toml) { '-dirty' } else { '' }
} finally {
	Pop-Location
}

if (Test-Path $Out) { [IO.Directory]::Delete($Out, $true) }
$stage = Join-Path $Out 'stage'
New-Item -ItemType Directory -Force $stage | Out-Null

# Платформа кеширует установленные компоненты в %APPDATA%\1C\1cv8\ExtCompT по имени файла из манифеста.
# С прежним именем клиент молча грузил бы старую библиотеку (например, без авторизации),
# поэтому в имя входит хеш содержимого: каждая сборка получает своё имя.
function Get-DllName([string]$Arch, [string]$Path) {
	$hash = (Get-FileHash $Path -Algorithm SHA256).Hash.Substring(0, 8).ToLower()
	"WebTransportAddIn_$Arch-$version-$hash.dll"
}
$builtX32 = Join-Path $Source 'target\i686-pc-windows-gnu\release\webtransport.dll'
$builtX64 = Join-Path $Source 'target\x86_64-pc-windows-gnu\release\webtransport.dll'
$dllX32 = Get-DllName 'x32' $builtX32
$dllX64 = Get-DllName 'x64' $builtX64
Copy-Item $builtX32 (Join-Path $stage $dllX32)
Copy-Item $builtX64 (Join-Path $stage $dllX64)

$manifest = @"
<?xml version="1.0" encoding="UTF-8"?>
<bundle xmlns="http://v8.1c.ru/8.2/addin/bundle" name="WebTransportAddIn" version="$version" description="WebSocket/HTTP add-in for 1C (Bearer auth)">
    <component os="Windows" path="$dllX32" type="native" arch="i386" />
    <component os="Windows" path="$dllX64" type="native" arch="x86_64" />
</bundle>
"@
[IO.File]::WriteAllText((Join-Path $stage 'Manifest.xml'), $manifest, (New-Object System.Text.UTF8Encoding($false)))

$zip = Join-Path $Out 'WebTransportAddIn.zip'
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip
"version=$version`ncommit=$commit$dirty`nx32=$dllX32`nx64=$dllX64`n" | Set-Content (Join-Path $Out 'BUILD.txt') -NoNewline

Write-Host "Готово: $zip (версия $version, коммит $commit$dirty, $dllX64)"
