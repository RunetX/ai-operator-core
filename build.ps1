<#
.SYNOPSIS
  Загружает расширение «ИИОператор» из исходников в файловую базу 1С. MCP-транспорт (внешняя компонента)
  уже лежит в расширении: Rust не нужен.

.DESCRIPTION
  Исходники хранятся в UTF-8 без BOM с LF. Скрипт копирует их в build/, приводит к формату выгрузки
  конфигуратора (UTF-8 с BOM, CRLF), загружает расширение через ibcmd и выключает для него безопасный
  режим и защиту от опасных действий: компоненте транспорта и журналу операций нужен привилегированный режим.

  Расширение заимствует справочники «Пользователи» и «Группы пользователей». Их идентификаторы в разных
  конфигурациях разные, поэтому скрипт выгружает эти два объекта из базы и подставляет их идентификаторы.

  С -Bot загружает ещё канал «бот в обсуждениях 1С» — расширение «ИИОператор_Бот». Ему нужны платформа 8.3.25
  и режим совместимости конфигурации не ниже 8.3.25, а в конфигурации — патч tools/install-bot-cfg.ps1.

  С -Tests загружает ещё расширение тестов «ИИОператор_Тесты» (нужен YAxUnit, см. test.ps1), вместе с -Bot —
  и тесты бота «ИИОператор_Бот_Тесты».
  ibcmd требует монопольного доступа: клиент 1С на этой базе должен быть закрыт.

.EXAMPLE
  ./build.ps1 -InfoBase 'C:\Bases\Acc' -User 'Администратор'
  ./build.ps1 -InfoBase 'C:\Bases\Acc' -User 'Администратор' -Tests
  ./build.ps1 -InfoBase 'C:\Bases\Trade' -User 'Администратор' -Bot
#>
param(
	[Parameter(Mandatory)]
	[string]$InfoBase,
	[string]$User = '',
	[string]$Password = '',
	[switch]$Tests,
	# Канал «бот в обсуждениях 1С»: расширение «ИИОператор_Бот».
	[switch]$Bot,
	# Версия платформы, например 8.3.27.2214. По умолчанию — самая новая 8.3: платформа 8.5 может
	# перевести файловую базу в свой формат, её указывают только явно.
	[string]$Platform = ''
)

$ErrorActionPreference = 'Stop'

function Find-PlatformFile([string]$Name) {
	if ($Platform) { return "C:\Program Files\1cv8\$Platform\bin\$Name" }
	$found = Get-ChildItem "C:\Program Files\1cv8\*\bin\$Name" -ErrorAction SilentlyContinue |
		Where-Object { $_.Directory.Parent.Name -match '^8\.3\.\d+\.\d+$' } |
		Sort-Object { [version]$_.Directory.Parent.Name } -Descending
	if (-not $found) { throw "Не найдена платформа 8.3 в C:\Program Files\1cv8. Укажите версию платформы: -Platform 8.3.27.2214" }
	return $found[0].FullName
}

$ibcmd = Find-PlatformFile 'ibcmd.exe'
if (-not (Test-Path $ibcmd)) { throw "Не найден ibcmd: $ibcmd" }
if (-not (Test-Path $InfoBase)) { throw "Не найден каталог базы: $InfoBase" }

$build = Join-Path $PSScriptRoot 'build'
$common = @("--db-path=$InfoBase", "--data=$(Join-Path $build 'ibcmd-data')")
if ($User) { $common += "--user=$User" }
$common += "--password=$Password"

function Invoke-Ibcmd([string[]]$Arguments) {
	# Пустой ввод: ibcmd не должен ждать логин или пароль с клавиатуры.
	$output = '' | & $ibcmd @Arguments @common 2>&1
	$output | ForEach-Object { "  $_" }
	if ($LASTEXITCODE -ne 0) { throw "ibcmd $($Arguments[0..1] -join ' ') завершился с кодом $LASTEXITCODE" }
}

function Install-Extension([string]$Name) {
	Invoke-Ibcmd @('config', 'apply', "--extension=$Name", '--force')
	Invoke-Ibcmd @('config', 'extension', 'update', "--name=$Name", '--safe-mode=no', '--unsafe-action-protection=no')
}

# Виды метаданных по каталогам выгрузки: для заимствованных объектов и порядка ChildObjects.
$kinds = @{ Languages = 'Language'; Catalogs = 'Catalog'; Documents = 'Document'; CommonModules = 'CommonModule'; Roles = 'Role';
	InformationRegisters = 'InformationRegister'; Enums = 'Enum'; DataProcessors = 'DataProcessor' }
# Конфигуратор ждёт ChildObjects в порядке видов метаданных.
$typeOrder = @('Language', 'Subsystem', 'StyleItem', 'CommonPicture', 'SessionParameter', 'Role', 'CommonTemplate',
	'FilterCriterion', 'CommonModule', 'CommonAttribute', 'ExchangePlan', 'XDTOPackage', 'WebService', 'HTTPService',
	'WSReference', 'EventSubscription', 'ScheduledJob', 'SettingsStorage', 'FunctionalOption',
	'FunctionalOptionsParameter', 'DefinedType', 'CommonCommand', 'CommandGroup', 'Constant', 'CommonForm', 'Catalog',
	'Document', 'DocumentNumerator', 'Sequence', 'DocumentJournal', 'Enum', 'Report', 'DataProcessor',
	'InformationRegister', 'AccumulationRegister', 'ChartOfCharacteristicTypes', 'ChartOfAccounts', 'AccountingRegister',
	'ChartOfCalculationTypes', 'BusinessProcess', 'Task')
$utf8Bom = New-Object System.Text.UTF8Encoding($true)

# Идентификаторы заимствованных объектов в основной конфигурации этой базы.
function Get-AdoptedIds([string]$Out) {
	$adopted = Get-ChildItem $Out -Filter *.xml -Recurse |
		Where-Object { $_.Directory.Parent.FullName -eq $Out -and (Select-String -Path $_.FullName -Pattern '<ObjectBelonging>Adopted</ObjectBelonging>' -Quiet) }
	$ids = @{}
	if (-not $adopted) { return $ids }
	$names = foreach ($file in $adopted) {
		$kind = $kinds[$file.Directory.Name]
		if (-not $kind) { throw "Заимствованный объект неизвестного вида: $($file.Directory.Name)\$($file.Name)" }
		"$kind.$($file.BaseName)"
	}
	$objects = Join-Path $build "config-objects\$([guid]::NewGuid().ToString('N').Substring(0, 8))"
	Write-Host ">> идентификаторы заимствованных объектов: $($names -join ', ')"
	# Вывод ibcmd — на экран: иначе он попал бы в результат функции.
	Invoke-Ibcmd (@('config', 'export', 'objects', "--out=$objects") + $names) | Out-Host
	foreach ($file in $adopted) {
		$main = Join-Path $objects "$($file.Directory.Name)\$($file.Name)"
		if (-not (Test-Path $main)) { throw "В конфигурации базы нет объекта $($file.Directory.Name)\$($file.BaseName)" }
		$id = [regex]::Match([IO.File]::ReadAllText($main), '<\w+ uuid="([0-9a-f-]{36})"').Groups[1].Value
		if (-not $id) { throw "Не найден идентификатор объекта в $main" }
		$ids[$file.FullName] = $id
	}
	return $ids
}

$projects = [ordered]@{ 'ai-operator' = 'ИИОператор' }
if ($Tests) { $projects['ai-operator-tests'] = 'ИИОператор_Тесты' }
if ($Bot) { $projects['ai-operator-bot'] = 'ИИОператор_Бот' }
if ($Bot -and $Tests) { $projects['ai-operator-bot-tests'] = 'ИИОператор_Бот_Тесты' }

foreach ($project in $projects.GetEnumerator()) {
	$src = Join-Path $PSScriptRoot "src\$($project.Key)"
	$out = Join-Path $build $project.Key
	$extension = $project.Value

	if (Test-Path $out) { [IO.Directory]::Delete($out, $true) }
	New-Item -ItemType Directory -Force $out | Out-Null
	Get-ChildItem $src -Recurse -File | ForEach-Object {
		$target = Join-Path $out $_.FullName.Substring($src.Length + 1)
		New-Item -ItemType Directory -Force (Split-Path $target) | Out-Null
		if ($_.Extension -in '.xml', '.bsl', '.txt') {
			$text = [IO.File]::ReadAllText($_.FullName) -replace "`r`n", "`n" -replace "`n", "`r`n"
			[IO.File]::WriteAllText($target, $text, $utf8Bom)
		} else {
			Copy-Item $_.FullName $target
		}
	}

	$ids = Get-AdoptedIds $out
	foreach ($item in $ids.GetEnumerator()) {
		$text = [IO.File]::ReadAllText($item.Key)
		$text = [regex]::Replace($text, '<ExtendedConfigurationObject>[^<]*</ExtendedConfigurationObject>',
			"<ExtendedConfigurationObject>$($item.Value)</ExtendedConfigurationObject>")
		[IO.File]::WriteAllText($item.Key, $text, $utf8Bom)
	}

	$configPath = Join-Path $out 'Configuration.xml'
	$config = [IO.File]::ReadAllText($configPath)
	$config = [regex]::Replace($config, '(?s)(<ChildObjects>\r\n)(.*?)(\r\n\t\t</ChildObjects>)', {
		param($m)
		$items = $m.Groups[2].Value -split "`r`n" | Where-Object { $_.Trim() }
		$sorted = $items | Sort-Object -Stable { $typeOrder.IndexOf(([regex]::Match($_, '<(\w+)>')).Groups[1].Value) }
		$m.Groups[1].Value + ($sorted -join "`r`n") + $m.Groups[3].Value
	})
	[IO.File]::WriteAllText($configPath, $config, $utf8Bom)

	Write-Host ">> $extension -> $InfoBase"
	Invoke-Ibcmd @('config', 'import', "--extension=$extension", $out)
	Install-Extension $extension
}
Write-Host 'Готово.'
