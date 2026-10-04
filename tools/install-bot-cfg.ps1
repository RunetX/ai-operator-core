<#
.SYNOPSIS
  Ставит в конфигурацию базы патч канала «бот в обсуждениях 1С»: бот «ИИОператор» с модулем-делегатом
  и пустой обработчик подбора пользователей в модуле управляемого приложения.

.DESCRIPTION
  Боты Системы взаимодействия до платформы 8.5.5 бывают только в конфигурации, а событие подбора
  пользователей расширение получает только как дополнение обработчика конфигурации. Логики в патче нет:
  модуль бота передаёт сообщение расширению «ИИОператор_Бот», без расширения бот ничего не делает.

  Скрипт выгружает из базы только корень конфигурации (Configuration.xml и модули приложения), дописывает
  бота и обработчик, если их ещё нет, загружает изменения частично и обновляет конфигурацию базы.
  Повторный запуск ничего не ломает: если патч на месте, загружается только модуль бота.
  После обновления типовой конфигурации патч накладывается заново этим же скриптом.

  Нужны платформа 8.3.25 или новее и конфигурация, которую можно менять: снятая с поддержки или на поддержке
  с разрешёнными изменениями. Добавление бота требует монопольного доступа: закройте все сеансы базы,
  у серверной базы заблокируйте начало сеансов в консоли кластера.

  -Check только проверяет, на месте ли патч.

.EXAMPLE
  ./tools/install-bot-cfg.ps1 -InfoBase 'C:\Bases\Trade' -User 'Администратор'
  ./tools/install-bot-cfg.ps1 -Server 'srv1c\trade' -User 'Администратор' -Password '<пароль>'
  ./tools/install-bot-cfg.ps1 -InfoBase 'C:\Bases\Trade' -User 'Администратор' -Check
#>
param(
	# Каталог файловой базы.
	[string]$InfoBase = '',
	# Серверная база в виде «сервер\база».
	[string]$Server = '',
	[string]$User = '',
	[string]$Password = '',
	[switch]$Check,
	# Версия платформы, например 8.3.27.2214. По умолчанию — самая новая 8.3: платформа 8.5 может
	# перевести файловую базу в свой формат, её указывают только явно.
	[string]$Platform = ''
)

$ErrorActionPreference = 'Stop'
if ([bool]$InfoBase -eq [bool]$Server) { throw 'Укажите базу: -InfoBase <каталог файловой базы> или -Server <сервер\база>.' }
if ($InfoBase -and -not (Test-Path $InfoBase)) { throw "Не найден каталог базы: $InfoBase" }

if ($Platform) {
	$designer = "C:\Program Files\1cv8\$Platform\bin\1cv8.exe"
} else {
	$found = Get-ChildItem 'C:\Program Files\1cv8\*\bin\1cv8.exe' -ErrorAction SilentlyContinue |
		Where-Object { $_.Directory.Parent.Name -match '^8\.3\.\d+\.\d+$' } |
		Sort-Object { [version]$_.Directory.Parent.Name } -Descending
	if (-not $found) { throw 'Не найдена платформа 8.3 в C:\Program Files\1cv8. Укажите версию платформы: -Platform 8.3.27.2214' }
	$designer = $found[0].FullName
}
if (-not (Test-Path $designer)) { throw "Не найден конфигуратор: $designer" }

$repo = Split-Path -Parent $PSScriptRoot
$src = Join-Path $repo 'src\ai-operator-bot-cfg'
$work = Join-Path $repo 'build\bot-cfg'
$utf8Bom = New-Object System.Text.UTF8Encoding($true)
$baseArg = if ($Server) { "/S`"$Server`"" } else { "/F`"$InfoBase`"" }

function Invoke-Designer([string]$Name, [string[]]$Arguments) {
	$log = Join-Path $work "$Name.log"
	$common = @('DESIGNER', $baseArg)
	if ($User) { $common += "/N`"$User`"" }
	$common += @("/P`"$Password`"", '/DisableStartupDialogs', '/DisableStartupMessages', "/Out`"$log`"")
	$process = Start-Process $designer -Wait -PassThru -ArgumentList ($common + $Arguments)
	Get-Content $log -Encoding UTF8 -ErrorAction SilentlyContinue | Where-Object { $_ } | ForEach-Object { "  $_" }
	if ($process.ExitCode -ne 0) { throw "Конфигуратор ($Name) завершился с кодом $($process.ExitCode), см. $log" }
}

function Write-Source([string]$From, [string]$To) {
	New-Item -ItemType Directory -Force (Split-Path $To) | Out-Null
	$text = [IO.File]::ReadAllText($From) -replace "`r`n", "`n" -replace "`n", "`r`n"
	[IO.File]::WriteAllText($To, $text, $utf8Bom)
}

if (Test-Path $work) { [IO.Directory]::Delete($work, $true) }
$dump = Join-Path $work 'dump'
$load = Join-Path $work 'load'
New-Item -ItemType Directory -Force $dump, $load | Out-Null

Write-Host '>> выгрузка корня конфигурации'
$dumpList = Join-Path $work 'dump-list.txt'
[IO.File]::WriteAllLines($dumpList, @('Configuration'), $utf8Bom)
Invoke-Designer 'dump' @('/DumpConfigToFiles', "`"$dump`"", '-listFile', "`"$dumpList`"")

$config = [IO.File]::ReadAllText((Join-Path $dump 'Configuration.xml'))
$modulePath = Join-Path $dump 'Ext\ManagedApplicationModule.bsl'
$module = [IO.File]::ReadAllText($modulePath)
$hasBot = $config -match '<Bot>ИИОператор</Bot>'
$hasHandler = $module -match 'Процедура\s+АвтоПодборПользователейСистемыВзаимодействия\s*\('
Write-Host "  бот ИИОператор: $(if ($hasBot) { 'есть' } else { 'нет' }); обработчик подбора: $(if ($hasHandler) { 'есть' } else { 'нет' })"
if ($Check) {
	if (-not ($hasBot -and $hasHandler)) { throw 'Патч конфигурации не на месте: запустите скрипт без -Check.' }
	Write-Host 'Патч на месте.'
	return
}

$files = New-Object System.Collections.Generic.List[string]
if (-not $hasBot) {
	# Бот встаёт после последнего бота конфигурации или в начало списка, если ботов нет.
	if ($config -match '<Bot>') {
		$config = [regex]::new('(?s)(.*)(\r\n)(\t+)(<Bot>[^<]+</Bot>)').Replace($config, '$1$2$3$4$2$3<Bot>ИИОператор</Bot>', 1)
	} else {
		$config = [regex]::new('(\r\n)(\t+)(<CommonModule>)').Replace($config, '$1$2<Bot>ИИОператор</Bot>$1$2$3', 1)
	}
}
$configOut = Join-Path $load 'Configuration.xml'
[IO.File]::WriteAllText($configOut, $config, $utf8Bom)
$files.Add($configOut)

if (-not $hasHandler) {
	$insert = ([IO.File]::ReadAllText((Join-Path $src 'ManagedApplicationModule.insert.bsl')) -replace "`r`n", "`n" -replace "`n", "`r`n").TrimEnd()
	$region = [regex]::new('(?s)(#Область ОбработчикиСобытий.*?)(\r\n#КонецОбласти)')
	if (-not $region.IsMatch($module)) { throw 'В модуле приложения нет области ОбработчикиСобытий: добавьте обработчик подбора вручную.' }
	$module = $region.Replace($module, { param($m) $m.Groups[1].Value + "`r`n" + $insert + "`r`n" + $m.Groups[2].Value }, 1)
	$moduleOut = Join-Path $load 'Ext\ManagedApplicationModule.bsl'
	New-Item -ItemType Directory -Force (Split-Path $moduleOut) | Out-Null
	[IO.File]::WriteAllText($moduleOut, $module, $utf8Bom)
	$files.Add($moduleOut)
}

foreach ($relative in 'Bots\ИИОператор.xml', 'Bots\ИИОператор\Ext\Module.bsl') {
	$target = Join-Path $load $relative
	Write-Source (Join-Path $src $relative) $target
	$files.Add($target)
}
$loadList = Join-Path $work 'load-list.txt'
[IO.File]::WriteAllLines($loadList, $files, $utf8Bom)

Write-Host '>> загрузка патча'
Invoke-Designer 'load' @('/LoadConfigFromFiles', "`"$load`"", '-listFile', "`"$loadList`"", '-partial')

Write-Host '>> обновление конфигурации базы'
if ($hasBot) {
	Invoke-Designer 'update' @('/UpdateDBCfg', '-Dynamic+')
} else {
	# Новый объект метаданных: динамическое обновление невозможно, нужен монопольный доступ.
	Invoke-Designer 'update' @('/UpdateDBCfg')
}
Write-Host 'Готово.'
