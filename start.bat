@echo off
setlocal
cd /d "%~dp0"

where java >nul 2>nul
if errorlevel 1 (
    echo JDK 25 ^(LTS^) is required. Install it and ensure java is on PATH.
    exit /b 1
)

where cargo >nul 2>nul
if errorlevel 1 (
    echo Rust ^(cargo^) is required. Install it from https://rustup.rs
    exit /b 1
)

if not exist xmr-btc-swap\swap\Cargo.toml (
    echo The xmr-btc-swap source is missing. Restore it before building.
    exit /b 1
)

echo Building native swap binaries and JNI library...
call scripts\prepare\prepare-windows.bat
if errorlevel 1 exit /b 1

if not exist app\src\main\resources\atomicswap.dll (
    echo Native build did not produce app\src\main\resources\atomicswap.dll.
    exit /b 1
)
if not exist app\src\main\resources\binaries\asb.exe (
    echo Native build did not produce the ASB executable.
    exit /b 1
)
if not exist app\src\main\resources\binaries\price_ticker.exe (
    echo Native build did not produce the price ticker executable.
    exit /b 1
)

echo Starting Outpost...
call gradlew.bat :app:run
exit /b %errorlevel%
