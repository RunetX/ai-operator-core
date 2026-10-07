@echo off
rem stdio-мост к MCP-серверу 1С для клиентов, которые запускают MCP-серверы командой
rem (Claude Desktop, LM Studio). Токен читается из файла пользователя и передаётся
rem mcp-remote через переменную окружения: его нет ни в конфигурации клиента, ни в командной строке.
rem Использование: mcp-bridge.cmd [порт]   (по умолчанию 9874)
setlocal
set "PORT=%~1"
if "%PORT%"=="" set "PORT=9874"
set "TOKEN_FILE=%LOCALAPPDATA%\AiOperator\mcp-token"
if not exist "%TOKEN_FILE%" (
	echo MCP token file not found: %TOKEN_FILE% 1>&2
	exit /b 1
)
set /p TOKEN=<"%TOKEN_FILE%"
set "AUTH_HEADER=Bearer %TOKEN%"
set "TOKEN="
npx -y mcp-remote@0.14.3 http://127.0.0.1:%PORT%/mcp --header "Authorization:${AUTH_HEADER}" --transport http-only
