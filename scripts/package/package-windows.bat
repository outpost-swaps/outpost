@echo off
setlocal

cd /d "%~dp0\..\.."

if not defined APP_VERSION set "APP_VERSION=0.1.0"
set "APP_NAME=Outpost"
if not defined DIST_DIR set "DIST_DIR=dist"

where jpackage >nul 2>nul
if errorlevel 1 (
  echo jpackage was not found on PATH. Add the JDK bin directory to PATH. 1>&2
  exit /b 1
)

call scripts\prepare\prepare-windows.bat
if errorlevel 1 exit /b %errorlevel%

call gradlew.bat :app:installDist
if errorlevel 1 exit /b %errorlevel%

if exist "%DIST_DIR%\%APP_NAME%" rmdir /s /q "%DIST_DIR%\%APP_NAME%"
if not exist "%DIST_DIR%" mkdir "%DIST_DIR%"

rem Use the Outpost icon. If jpackage rejects it, run this whole script again without the icon
rem (the cleanup above clears any leftovers): a package with the default icon beats no package.
set "ICON_ARG=--icon scripts\package\icons\Outpost.ico"
if defined NO_ICON set "ICON_ARG="
jpackage ^
  --type app-image ^
  --input app\build\install\app\lib ^
  --main-jar app.jar ^
  --main-class swap.gui.Main ^
  --name "%APP_NAME%" ^
  %ICON_ARG% ^
  --app-version "%APP_VERSION%" ^
  --vendor "Outpost" ^
  --description "Tor-only BTC/XMR atomic swaps" ^
  --dest "%DIST_DIR%"
if errorlevel 1 goto icon_failed
goto after_jpackage

:icon_failed
rem (labels, not a parenthesized block: %errorlevel% inside a block is read before the block runs)
if defined NO_ICON exit /b 1
echo WARNING: building with the Outpost icon failed; building again without it 1>&2
set NO_ICON=1
call "%~f0"
exit /b %errorlevel%

:after_jpackage

echo Created %DIST_DIR%\%APP_NAME% (unsigned; sign/checksum before distribution)
