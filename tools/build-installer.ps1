<#
.SYNOPSIS
  Собирает установочную обработку ИИ-оператора (.epf) из исходников src/ai-operator-installer.

.DESCRIPTION
  Конфигуратор загружает XML внешней обработки в .epf (/LoadExternalDataProcessorOrReportFromFiles). Ему нужна
  любая файловая информационная база (-InfoBase, -User), она не меняется.
  Версия обработки (ВерсияОбработки в модуле объекта) должна совпадать с версией ядра в
  src/ai-operator/Configuration.xml: обработка выходит с релизом ядра.
  -Check - сверка со стандартами (check-std.ps1). Компиляцию модулей проверяет открытие обработки: e2e
  tests/e2e/test_installer.py создает ее через COM, форму открывает живая проверка.

.EXAMPLE
  ./tools/build-installer.ps1 -InfoBase C:\Bases\Demo -User Администратор
  ./tools/build-installer.ps1 -InfoBase C:\Bases\Demo -User Администратор -Check -Out build\installer.epf
#>
param(
	[Parameter(Mandatory)][string]$InfoBase,
	[string]$User = '',
	[string]$Platform = '8.3.27.2214',
	[string]$Out = '',
	[switch]$Check
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$src = Join-Path $repo 'src\ai-operator-installer'
$module = Join-Path $src 'УстановкаИИОператора\Ext\ObjectModule.bsl'

$version = [regex]::Match([IO.File]::ReadAllText($module),
	'Функция ВерсияОбработки\(\) Экспорт\s+Возврат "([^"]+)"').Groups[1].Value
if (-not $version) { throw "В $module не найдена ВерсияОбработки" }
$core = [regex]::Match([IO.File]::ReadAllText((Join-Path $repo 'src\ai-operator\Configuration.xml')),
	'<Version>([^<]+)</Version>').Groups[1].Value
if ($version -ne $core) { throw "ВерсияОбработки «$version», а версия ядра ${core}: обработка выходит с релизом ядра" }

if (-not $Out) { $Out = Join-Path $repo "build\release\ai-operator-installer-$version.epf" }
$Out = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Out)
New-Item -ItemType Directory -Force (Split-Path $Out) | Out-Null
if (Test-Path $Out) { Remove-Item $Out -Force }

$designer = "C:\Program Files\1cv8\$Platform\bin\1cv8.exe"
$log = Join-Path $repo 'build\installer.log'
New-Item -ItemType Directory -Force (Split-Path $log) | Out-Null
Write-Host ">> installer $version -> $Out"
$process = Start-Process -FilePath $designer -Wait -PassThru -ArgumentList @('DESIGNER', "/F`"$InfoBase`"",
	"/N`"$User`"", '/P""', '/DisableStartupDialogs', '/DisableStartupMessages', "/Out`"$log`"",
	'/LoadExternalDataProcessorOrReportFromFiles', "`"$(Join-Path $src 'УстановкаИИОператора.xml')`"", "`"$Out`"")
Get-Content $log -Encoding utf8 -ErrorAction SilentlyContinue | Where-Object { $_ } | ForEach-Object { "  $_" }
if ($process.ExitCode -ne 0 -or -not (Test-Path $Out)) { throw "Конфигуратор не собрал обработку, см. $log" }

if ($Check) {
	Write-Host '>> standards ai-operator-installer'
	& (Join-Path $PSScriptRoot 'check-std.ps1') -Project 'ai-operator-installer'
}
Write-Host "Готово: $Out"
