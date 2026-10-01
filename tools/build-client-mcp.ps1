<#
.SYNOPSIS
  Собирает client_mcp.cfe с проверкой Bearer-токена: берёт релизный client_mcp.cfe проекта
  onec-client-mcp-devkit и заменяет в нём только макет компоненты Мсп_webTransport. BSL-код не меняется.

.DESCRIPTION
  Релиз скачивается с GitHub и сверяется по SHA-256. Компонента собирается tools/build-addin.ps1,
  если её ещё нет, если сменился закреплённый коммит форка или указан -RebuildAddin. Замена идёт через
  временную пустую файловую базу в build/client-mcp/ib: ibcmd config load -> config export (XML) ->
  замена Template.bin -> config import -> config save. Ваши базы не затрагиваются.

  Результат — build/client_mcp-0.6.5-auth.cfe. Установка: ./build.ps1 -InfoBase ... -ClientMcp <файл>.

.EXAMPLE
  ./tools/build-client-mcp.ps1
  ./tools/build-client-mcp.ps1 -RebuildAddin
#>
param(
	# Версия платформы, например 8.3.27.2214. По умолчанию — самая новая 8.3.
	[string]$Platform = '',
	[switch]$RebuildAddin
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$release = 'v0.6.5'
$releaseUrl = "https://github.com/1c-neurofish/onec-client-mcp-devkit/releases/download/$release/client_mcp.cfe"
$releaseSha256 = 'd1093475a15e50a33ad48a64b61d09d1108b5a39328c73e6be17a5c914825e7f'
$extension = 'client_mcp'
$work = Join-Path $repo 'build\client-mcp'
$dist = Join-Path $repo "build\dist\client_mcp-$release.cfe"
$addinZip = Join-Path $repo 'build\addin\WebTransportAddIn.zip'
$out = Join-Path $repo "build\client_mcp-$($release.TrimStart('v'))-auth.cfe"

if ($Platform) {
	$ibcmd = "C:\Program Files\1cv8\$Platform\bin\ibcmd.exe"
} else {
	$found = Get-ChildItem 'C:\Program Files\1cv8\*\bin\ibcmd.exe' -ErrorAction SilentlyContinue |
		Where-Object { $_.Directory.Parent.Name -match '^8\.3\.\d+\.\d+$' } |
		Sort-Object { [version]$_.Directory.Parent.Name } -Descending
	if (-not $found) { throw 'Не найдена платформа 8.3 в C:\Program Files\1cv8. Укажите версию: -Platform 8.3.27.2214' }
	$ibcmd = $found[0].FullName
}
if (-not (Test-Path $ibcmd)) { throw "Не найден ibcmd: $ibcmd" }

if (-not (Test-Path $dist)) {
	Write-Host ">> download $releaseUrl"
	New-Item -ItemType Directory -Force (Split-Path $dist) | Out-Null
	Invoke-WebRequest -Uri $releaseUrl -OutFile $dist -UseBasicParsing
}
$hash = (Get-FileHash $dist -Algorithm SHA256).Hash.ToLower()
if ($hash -ne $releaseSha256) {
	throw "SHA-256 файла $dist не совпадает с релизом $release ($hash). Удалите файл и запустите скрипт снова"
}

# Компонента собирается, если ее еще нет или после обновления репозитория сменился закрепленный коммит форка.
& (Join-Path $PSScriptRoot 'build-addin.ps1') -IfChanged:(-not $RebuildAddin)

if (Test-Path $work) { [IO.Directory]::Delete($work, $true) }
$ib = Join-Path $work 'ib'
$xml = Join-Path $work 'xml'
$data = Join-Path $work 'data'
New-Item -ItemType Directory -Force $work | Out-Null

function Invoke-Ibcmd([string[]]$Arguments) {
	# Пустой ввод: ibcmd не должен ждать логин или пароль с клавиатуры.
	$output = '' | & $ibcmd @Arguments 2>&1
	$output | ForEach-Object { "  $_" }
	if ($LASTEXITCODE -ne 0) { throw "ibcmd $($Arguments -join ' ') завершился с кодом $LASTEXITCODE" }
}

$db = @("--db-path=$ib", "--data=$data")

Write-Host '>> временная база'
Invoke-Ibcmd (@('infobase', 'create', '--create-database') + $db)
Write-Host ">> load $dist"
Invoke-Ibcmd (@('config', 'load', "--extension=$extension") + $db + @($dist))
Write-Host '>> export XML'
Invoke-Ibcmd (@('config', 'export', "--extension=$extension") + $db + @($xml))

$template = Join-Path $xml 'CommonTemplates\Мсп_webTransport\Ext\Template.bin'
if (-not (Test-Path $template)) { throw "Не найден макет компоненты: $template" }
Write-Host '>> замена макета Мсп_webTransport'
Copy-Item $addinZip $template -Force

Write-Host '>> import XML'
Invoke-Ibcmd (@('config', 'import', "--extension=$extension") + $db + @($xml))
Write-Host ">> save $out"
Invoke-Ibcmd (@('config', 'save', "--extension=$extension") + $db + @($out))

Write-Host "Готово: $out"
Write-Host "  sha256 $((Get-FileHash $out -Algorithm SHA256).Hash.ToLower())"
Write-Host "  компонента: $((Get-Content (Join-Path $repo 'build\addin\BUILD.txt')) -join ', ')"
