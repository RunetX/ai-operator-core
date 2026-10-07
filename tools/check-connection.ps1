<#
.SYNOPSIS
  Проверяет, готов ли MCP-сервер ИИ-оператора к подключению клиента. На первой проблеме говорит, что сделать.

.DESCRIPTION
  Проверки по порядку:
    1. файл токена %LOCALAPPDATA%\WebTransport\mcp-token;
    2. порт на 127.0.0.1 слушает 1С;
    3. запрос без токена получает 401;
    4. initialize с токеном;
    5. tools/list: число инструментов, среди них ping;
    6. ping: версия расширения, конфигурация, платформа.
  Скрипт останавливается на первом провале и называет следующий шаг из «Если не работает» в docs/clients.md.
  Токен на экран не выводится. Скрипт ничего не меняет; вызов ping попадает в журнал регистрации 1С,
  как любой вызов инструмента.
  Работает в Windows PowerShell 5.1 и PowerShell 7. Файл сохранён в UTF-8 с BOM: без BOM
  Windows PowerShell 5.1 читает кириллицу неверно.

.EXAMPLE
  ./tools/check-connection.ps1
  ./tools/check-connection.ps1 -Port 9875
#>
param(
	[int]$Port = 9874,
	[int]$TimeoutSec = 90
)

$ErrorActionPreference = 'Stop'
$ProtocolVersion = '2025-06-18'
$url = "http://127.0.0.1:$Port/mcp"
$startCommand = '«ИИ-оператор → Запустить ИИ-оператор»'

function Write-Ok([string]$Text) {
	Write-Host "[ OK ] $Text" -ForegroundColor Green
}

function Write-Warn([string]$Text, [string]$Action) {
	Write-Host "[ !! ] $Text" -ForegroundColor Yellow
	Write-Host "       Что сделать: $Action"
}

function Stop-Check([string]$Problem, [string]$Action) {
	Write-Host "[FAIL] $Problem" -ForegroundColor Red
	Write-Host "       Что сделать: $Action"
	exit 1
}

Add-Type -AssemblyName System.Net.Http
$http = New-Object System.Net.Http.HttpClient
$http.Timeout = [TimeSpan]::FromSeconds($TimeoutSec)
$script:SessionId = $null
$script:RequestId = 0

# Один запрос JSON-RPC по Streamable HTTP. Возвращает код HTTP и ответ на этот запрос;
# вместо кода — 'timeout' или 'unreachable', если ответа нет.
function Invoke-Mcp([string]$Method, $Params, [string]$Token, [switch]$Notification) {
	$payload = [ordered]@{ jsonrpc = '2.0'; method = $Method }
	if (-not $Notification) {
		$script:RequestId++
		$payload.id = $script:RequestId
	}
	if ($null -ne $Params) { $payload.params = $Params }

	$request = New-Object System.Net.Http.HttpRequestMessage([System.Net.Http.HttpMethod]::Post, $url)
	$json = $payload | ConvertTo-Json -Depth 10 -Compress
	$request.Content = New-Object System.Net.Http.StringContent($json, [Text.Encoding]::UTF8, 'application/json')
	[void]$request.Headers.TryAddWithoutValidation('Accept', 'application/json, text/event-stream')
	[void]$request.Headers.TryAddWithoutValidation('MCP-Protocol-Version', $ProtocolVersion)
	if ($script:SessionId) { [void]$request.Headers.TryAddWithoutValidation('Mcp-Session-Id', $script:SessionId) }
	if ($Token) { [void]$request.Headers.TryAddWithoutValidation('Authorization', "Bearer $Token") }

	try {
		$response = $http.SendAsync($request).GetAwaiter().GetResult()
	} catch {
		$exception = $_.Exception
		while ($exception) {
			if ($exception -is [Threading.Tasks.TaskCanceledException] -or $exception -is [TimeoutException]) {
				return @{ Status = 'timeout' }
			}
			$last = $exception
			$exception = $exception.InnerException
		}
		return @{ Status = 'unreachable'; Error = $last.Message }
	}

	$values = $null
	if ($response.Headers.TryGetValues('Mcp-Session-Id', [ref]$values)) { $script:SessionId = @($values)[0] }
	# Кодировку задаём сами: без charset в Content-Type HttpClient прочитал бы кириллицу как Latin-1.
	$body = [Text.Encoding]::UTF8.GetString($response.Content.ReadAsByteArrayAsync().GetAwaiter().GetResult())
	$result = @{ Status = [int]$response.StatusCode; Message = $null }
	if ($Notification -or -not $body.Trim()) { return $result }

	$messages = @()
	$mediaType = if ($response.Content.Headers.ContentType) { $response.Content.Headers.ContentType.MediaType } else { '' }
	if ($mediaType -eq 'text/event-stream') {
		foreach ($block in ($body -replace "`r`n", "`n") -split "`n`n") {
			$data = ($block -split "`n" | Where-Object { $_ -like 'data:*' } | ForEach-Object { $_.Substring(5).TrimStart() }) -join "`n"
			if ($data) { $messages += $data | ConvertFrom-Json }
		}
	} elseif ($body.TrimStart().StartsWith('{') -or $body.TrimStart().StartsWith('[')) {
		$messages = @($body | ConvertFrom-Json)
	}
	$result.Message = $messages | Where-Object { $_.id -eq $script:RequestId } | Select-Object -First 1
	return $result
}

function Stop-OnNoAnswer($Result) {
	if ($Result.Status -eq 'timeout') {
		Stop-Check "1С не ответила за $TimeoutSec с" ("подождать минуту и повторить: после запуска файловая база выполняет фоновые задания, " +
			"и запросы ждут в очереди. Если не помогло — посмотреть, не ждёт ли 1С нажатия в открытом окне")
	}
	if ($Result.Status -eq 'unreachable') {
		Stop-Check "Не удалось соединиться с $url ($($Result.Error))" "запустить ИИ-оператор командой $startCommand и повторить проверку"
	}
}

function Stop-OnRpcError($Result, [string]$Method) {
	if (-not $Result.Message) { Stop-Check "Сервер не ответил на $Method (HTTP $($Result.Status))" "перезапустить ИИ-оператор в 1С и повторить проверку" }
	if ($Result.Message.error) {
		Stop-Check "Сервер вернул ошибку на ${Method}: $($Result.Message.error.message)" "перезапустить ИИ-оператор в 1С; если ошибка повторяется — посмотреть события ИИОператор.* в журнале регистрации 1С"
	}
}

# 1. Токен
$tokenFile = Join-Path $env:LOCALAPPDATA 'WebTransport\mcp-token'
$token = if (Test-Path $tokenFile) { [IO.File]::ReadAllText($tokenFile).Trim() } else { '' }
if (-not $token) {
	Stop-Check "Нет токена: файл $tokenFile не найден или пуст" "выполнить ./tools/new-mcp-token.ps1, затем перезапустить ИИ-оператор в 1С"
}
Write-Ok "Токен есть: $tokenFile"
if ($env:ONEC_MCP_TOKEN -and $env:ONEC_MCP_TOKEN.Trim() -cne $token) {
	Write-Warn "Переменная ONEC_MCP_TOKEN в этом окне не совпадает с файлом токена: Claude Code, запущенный отсюда, получит 401" `
		'$env:ONEC_MCP_TOKEN = (Get-Content "$env:LOCALAPPDATA\WebTransport\mcp-token" -Raw).Trim()'
}

# 2. Порт
$listen = @(Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue)
if (-not $listen) {
	$onec = @(Get-Process -Name '1cv8c', '1cv8' -ErrorAction SilentlyContinue)
	if (-not $onec) {
		Stop-Check "Порт $Port никто не слушает, 1С не запущена" "запустить 1С и в ней ИИ-оператор командой $startCommand"
	}
	$onecPorts = @(Get-NetTCPConnection -State Listen -LocalAddress 127.0.0.1 -ErrorAction SilentlyContinue |
		Where-Object { $onec.Id -contains $_.OwningProcess } | Select-Object -ExpandProperty LocalPort -Unique)
	if ($onecPorts) {
		Stop-Check "Порт $Port никто не слушает, а 1С слушает порт $($onecPorts -join ', ')" ("сверить порт в «Настройках ИИ-оператора» " +
			"и проверить с ним: ./tools/check-connection.ps1 -Port $($onecPorts[0]). Тот же порт должен быть в конфигурации MCP-клиента")
	}
	Stop-Check "1С запущена, но порт $Port никто не слушает" ("запустить ИИ-оператор командой $startCommand. " +
		"Если после установки или обновления 1С показывает окно «Внешняя компонента успешно установлена», нажать в нём «OK»")
}
$owner = Get-Process -Id $listen[0].OwningProcess -ErrorAction SilentlyContinue
$ownerName = if ($owner) { $owner.ProcessName } else { "pid $($listen[0].OwningProcess)" }
if ($owner -and $owner.ProcessName -notlike '1cv8*') {
	Stop-Check "Порт $Port занят другой программой: $ownerName" ("указать другой порт в «Настройках ИИ-оператора», перезапустить ИИ-оператор " +
		"и проверить с этим портом: ./tools/check-connection.ps1 -Port <порт>")
}
$addresses = @($listen | Select-Object -ExpandProperty LocalAddress -Unique)
if ($addresses | Where-Object { $_ -ne '127.0.0.1' }) {
	Write-Warn "Порт $Port открыт не только для этого компьютера: $($addresses -join ', ')" "запускать сервер командой ${startCommand}: она слушает только 127.0.0.1"
} else {
	Write-Ok "Порт $Port на 127.0.0.1 слушает $ownerName"
}

# 3. Без токена
$initialize = @{
	protocolVersion = $ProtocolVersion
	capabilities = @{}
	clientInfo = @{ name = 'check-connection'; version = '1' }
}
$anonymous = Invoke-Mcp 'initialize' $initialize -Token ''
Stop-OnNoAnswer $anonymous
if ($anonymous.Status -ge 200 -and $anonymous.Status -lt 300) {
	Stop-Check "Сервер принимает запросы без токена" ("остановить сервер и запустить его командой ${startCommand}: она проверяет авторизацию при запуске. " +
		"Если на порту работает client_mcp прежней установки, остановить его и удалить расширение client_mcp: ядру он больше не нужен")
}
if ($anonymous.Status -ne 401) {
	Stop-Check "Без токена сервер ответил HTTP $($anonymous.Status), а ожидался 401" "проверить, что порт $Port принадлежит ИИ-оператору, и перезапустить его в 1С"
}
Write-Ok 'Без токена сервер отвечает 401'
$script:SessionId = $null

# 4. С токеном
$init = Invoke-Mcp 'initialize' $initialize -Token $token
Stop-OnNoAnswer $init
if ($init.Status -eq 401) {
	Stop-Check 'Сервер не принял токен из файла' ("токен перевыпустили, а сервер работает со старым: остановить и снова запустить ИИ-оператор в 1С, " +
		"затем перезапустить MCP-клиент")
}
Stop-OnRpcError $init 'initialize'
$server = $init.Message.result.serverInfo
Write-Ok "initialize: $($server.name) $($server.version), протокол $($init.Message.result.protocolVersion)"
[void](Invoke-Mcp 'notifications/initialized' $null -Token $token -Notification)

# 5. Инструменты
$list = Invoke-Mcp 'tools/list' $null -Token $token
Stop-OnNoAnswer $list
Stop-OnRpcError $list 'tools/list'
$tools = @($list.Message.result.tools)
if (-not ($tools | Where-Object { $_.name -eq 'ping' })) {
	Stop-Check "Сервер отвечает, но инструментов ИИ-оператора нет (всего инструментов: $($tools.Count))" ("проверить, что расширение «ИИОператор» " +
		"установлено и включено, и запустить сервер командой $startCommand")
}
Write-Ok "tools/list: инструментов $($tools.Count)"

# 6. ping. Вызов инструмента ждёт, пока 1С освободится, поэтому время ответа показывает её занятость.
$started = [Diagnostics.Stopwatch]::StartNew()
$ping = Invoke-Mcp 'tools/call' @{ name = 'ping'; arguments = @{} } -Token $token
Stop-OnNoAnswer $ping
Stop-OnRpcError $ping 'ping'
$text = (@($ping.Message.result.content) | Where-Object { $_.type -eq 'text' } | ForEach-Object { $_.text }) -join ''
$info = $text | ConvertFrom-Json
if ($info.error) {
	Stop-Check "ping вернул ошибку $($info.error.code): $($info.error.message)" $(if ($info.error.hint) { $info.error.hint } else { 'посмотреть события ИИОператор.* в журнале регистрации 1С' })
}
Write-Ok "ping: ИИ-оператор $($info.extension_version), $($info.configuration) $($info.configuration_version), платформа $($info.platform)"
$seconds = [int]$started.Elapsed.TotalSeconds
if ($seconds -ge 2) {
	Write-Warn "1С ответила на ping за $seconds с" "подождать, если 1С только что запущена: файловая база выполняет фоновые задания. Иначе закрыть в 1С долгие операции"
}

Write-Host ''
Write-Host "Подключение работает. Адрес для MCP-клиента: $url, настройка клиентов — docs/clients.md"
exit 0
