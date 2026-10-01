<#
.SYNOPSIS
  Запускает тесты YAxUnit ядра (расширение «ИИОператор_Тесты») в файловой базе 1С.

.DESCRIPTION
  Нужны YAxUnit 25.12 без безопасного режима, client_mcp, ядро и расширение тестов (./build.ps1 -Tests).
  Тесты рассчитаны на демо-базу «Бухгалтерии предприятия 3.0»: часть проверок поиска, показа и отчётов
  опирается на её данные. Тесты создают данные в транзакции и откатывают её. Отчёт — build/tests/junit.xml.

  Клиент 1С на этой базе должен быть закрыт: тесты запускают свой сеанс.

.EXAMPLE
  ./test.ps1 -InfoBase 'C:\Bases\Acc' -User 'Администратор'
#>
param(
	[Parameter(Mandatory)]
	[string]$InfoBase,
	[string]$User = '',
	[string]$Password = '',
	# Версия платформы, например 8.3.27.2214. По умолчанию — самая новая 8.3: платформа 8.5 может
	# перевести файловую базу в свой формат, её указывают только явно.
	[string]$Platform = '',
	[int]$TimeoutSec = 900
)

$ErrorActionPreference = 'Stop'

if ($Platform) {
	$client = "C:\Program Files\1cv8\$Platform\bin\1cv8c.exe"
} else {
	$found = Get-ChildItem 'C:\Program Files\1cv8\*\bin\1cv8c.exe' -ErrorAction SilentlyContinue |
		Where-Object { $_.Directory.Parent.Name -match '^8\.3\.\d+\.\d+$' } |
		Sort-Object { [version]$_.Directory.Parent.Name } -Descending
	if (-not $found) { throw 'Не найдена платформа 8.3 в C:\Program Files\1cv8. Укажите версию платформы: -Platform 8.3.27.2214' }
	$client = $found[0].FullName
}
if (-not (Test-Path $client)) { throw "Не найден клиент 1С: $client" }

$out = Join-Path $PSScriptRoot 'build\tests'
if (Test-Path $out) { [IO.Directory]::Delete($out, $true) }
New-Item -ItemType Directory -Force $out | Out-Null
$report = Join-Path $out 'junit.xml'
$exitCodeFile = Join-Path $out 'exit-code.txt'
$configFile = Join-Path $out 'config.json'
$config = [ordered]@{
	filter = @{ extensions = @('ИИОператор_Тесты') }
	reportFormat = 'jUnit'
	reportPath = $report
	closeAfterTests = $true
	showReport = $false
	exitCode = $exitCodeFile
}
[IO.File]::WriteAllText($configFile, ($config | ConvertTo-Json -Depth 5), (New-Object System.Text.UTF8Encoding($false)))

$arguments = @('ENTERPRISE', "/F`"$InfoBase`"")
if ($User) { $arguments += "/N`"$User`"" }
$arguments += @("/P`"$Password`"", '/DisableStartupDialogs', '/DisableStartupMessages', "/C`"RunUnitTests=$configFile`"")

Write-Host ">> YAxUnit -> $InfoBase"
$process = Start-Process -FilePath $client -PassThru -ArgumentList $arguments
if (-not $process.WaitForExit($TimeoutSec * 1000)) {
	Stop-Process -Id $process.Id -Force
	throw "Тесты не завершились за $TimeoutSec с. Если в базе нет YAxUnit, клиент молча ждёт: установите YAxUnit 25.12"
}
if (-not (Test-Path $report)) { throw "Нет отчёта ${report}: тесты не запустились (код клиента $($process.ExitCode))" }

[xml]$junit = Get-Content $report -Raw -Encoding utf8
$cases = $junit.SelectNodes('//testcase')
$failed = @($cases | Where-Object { $_.failure -or $_.error })
$skipped = @($cases | Where-Object { $_.skipped })
foreach ($case in $failed) {
	$node = if ($case.failure) { $case.failure } else { $case.error }
	$message = if ($node -is [string]) { $node } else { $node.message }
	Write-Host "  FAIL $($case.classname) :: $($case.name)"
	Write-Host "       $message"
}
Write-Host ("Тестов: {0}, упало: {1}, пропущено: {2}" -f $cases.Count, $failed.Count, $skipped.Count)

$exitCode = if (Test-Path $exitCodeFile) { (Get-Content $exitCodeFile -Raw).Trim() } else { '' }
if ($failed.Count -gt 0 -or ($exitCode -and $exitCode -ne '0')) { throw "Тесты не прошли (код YAxUnit: $exitCode)" }
Write-Host 'Готово.'
