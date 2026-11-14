@echo off
rem ============================================================
rem  Compila acelerador.exe no Windows 10/11.
rem  Instala automaticamente o que faltar:
rem    1) Rust (rustup/cargo) via winget
rem    2) Visual Studio Build Tools com C++ via winget
rem  Depois baixa as dependencias do projeto (crates) e compila.
rem  Requer winget (App Installer), presente no Windows 10 1809+ atualizado.
rem ============================================================
setlocal
title Compilar acelerador.exe
cd /d "%~dp0"

set "CARGO_BIN=%USERPROFILE%\.cargo\bin"
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"

echo.
echo === 1/4 Verificando Rust ===
set "PATH=%CARGO_BIN%;%PATH%"
where cargo >nul 2>&1
if not errorlevel 1 goto rust_ok

echo Rust nao encontrado. Instalando via winget...
winget install --id Rustlang.Rustup -e --silent --accept-package-agreements --accept-source-agreements
if errorlevel 1 goto erro_rust_winget
set "PATH=%CARGO_BIN%;%PATH%"

:rust_ok
rem "rustup default" tambem baixa a toolchain estavel se ela ainda nao existir.
call rustup default stable-msvc
if errorlevel 1 goto erro_rust
echo Rust OK.

echo.
echo === 2/4 Verificando Visual Studio Build Tools (C++) ===
set "VSPATH="
if exist "%VSWHERE%" for /f "usebackq delims=" %%i in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VSPATH=%%i"
if defined VSPATH goto vs_ok

echo Build Tools nao encontrado. Instalando via winget (pode demorar e ocupar varios GB)...
winget install --id Microsoft.VisualStudio.2022.BuildTools -e --accept-package-agreements --accept-source-agreements --override "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
if errorlevel 1 goto erro_vs

:vs_ok
echo Build Tools OK.

echo.
echo === 3/4 Baixando dependencias e compilando (cargo build --release) ===
cargo build --release
if errorlevel 1 goto erro_build

echo.
echo === 4/4 Copiando acelerador.exe para esta pasta ===
copy /Y "target\release\acelerador.exe" "acelerador.exe" >nul
if errorlevel 1 goto erro_build

echo.
echo PRONTO: %CD%\acelerador.exe
goto fim

:erro_rust_winget
echo.
echo Falha ao instalar o Rust via winget.
echo Instale manualmente em https://rustup.rs e rode este arquivo de novo.
goto fim

:erro_rust
echo.
echo O Rust nao ficou disponivel nesta janela.
echo Feche esta janela, abra outra e rode este arquivo de novo.
goto fim

:erro_vs
echo.
echo Falha ao instalar o Visual Studio Build Tools.
echo Instale manualmente com a carga de trabalho "Desenvolvimento para desktop com C++" e rode de novo.
goto fim

:erro_build
echo.
echo Falha na compilacao. Veja as mensagens acima.
goto fim

:fim
echo.
pause
endlocal
