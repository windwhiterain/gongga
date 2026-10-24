@echo off
rem Run a command in WSL, from a Windows directory, without leaving the Windows shell.
rem
rem Why a wrapper: WSL and Git Bash must never share the name `bash`. Git Bash owns
rem `bash`; Linux is reached through `wsl`, and this file makes that convenient by
rem translating the working directory and dropping into the same path inside Linux.
rem
rem   lx                    -> interactive bash in the current directory
rem   lx make test          -> run one command there
setlocal enabledelayedexpansion
set "DIR=%CD%"
set "LXDIR="

rem C:\resource\gongga -> /mnt/c/resource/gongga ; also handles /c/... from Git Bash.
if "%DIR:~1,2%"==":\" (
  set "DRIVE=%DIR:~0,1%"
  call :lower DRIVE
  set "REST=%DIR:~2%"
  set "REST=!REST:\=/!"
  set "LXDIR=/mnt/!DRIVE!!REST!"
) else (
  echo %DIR% | findstr /b /c:"/c/" >nul && (
    set "REST=%DIR:~2%"
    set "LXDIR=/mnt/c!REST!"
  )
)
if not defined LXDIR set "LXDIR=%DIR%"

rem No command: interactive shell in that directory.
if "%~1"=="" (
  wsl.exe -e bash -lc "cd '%LXDIR%' 2>/dev/null || true; exec bash -l"
  exit /b %ERRORLEVEL%
)

rem One or more words: quote the lot so bash sees the command as written.
set "ARGS=%*"
wsl.exe -e bash -lc "cd '%LXDIR%' 2>/dev/null || true; %ARGS%"
exit /b %ERRORLEVEL%

:lower
rem Sets the variable named by %1 to its lowercase form.
for %%A in (a b c d e f g h i j k l m n o p q r s t u v w x y z) do (
  set "%1=!%1:%%A=%%A!"
)
goto :eof
