<#
.SYNOPSIS
  Создаёт токен авторизации MCP в файле пользователя %LOCALAPPDATA%\WebTransport\mcp-token.

.DESCRIPTION
  Компонента из форка читает этот файл при каждом запуске MCP-сервера и с этого момента принимает
  только запросы с заголовком "Authorization: Bearer <токен>". Токен не выводится на экран:
  клиенты читают его из того же файла или из переменной окружения ONEC_MCP_TOKEN.
  После перевыпуска токена сервер в 1С нужно перезапустить.
  Работает в Windows PowerShell 5.1 и PowerShell 7. Файл сохранён в UTF-8 с BOM: без BOM
  Windows PowerShell 5.1 читает кириллицу неверно.

.EXAMPLE
  ./tools/new-mcp-token.ps1
  ./tools/new-mcp-token.ps1 -Force
#>
param(
	[switch]$Force
)

$ErrorActionPreference = 'Stop'
$path = Join-Path $env:LOCALAPPDATA 'WebTransport\mcp-token'

if ((Test-Path $path) -and -not $Force) {
	Write-Host "Токен уже есть: $path (перевыпустить: -Force)"
	return
}

$bytes = New-Object byte[] 32
[Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
$token = [Convert]::ToBase64String($bytes).TrimEnd('=').Replace('+', '-').Replace('/', '_')

New-Item -ItemType Directory -Force (Split-Path $path) | Out-Null
[IO.File]::WriteAllText($path, $token, (New-Object System.Text.UTF8Encoding($false)))
Write-Host "Токен записан: $path ($($token.Length) символов). Перезапустите MCP-сервер в 1С."
