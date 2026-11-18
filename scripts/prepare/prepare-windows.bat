cd xmr-btc-swap
cargo build --release
if errorlevel 1 exit /b 1
cargo build --release -p swap --bin asb --features protocol-v5
if errorlevel 1 exit /b 1
if not exist ..\app\src\main\resources\binaries mkdir ..\app\src\main\resources\binaries
copy target\release\asb.exe ..\app\src\main\resources\binaries\asb.exe
if errorlevel 1 exit /b 1
copy target\release\price_ticker.exe ..\app\src\main\resources\binaries\price_ticker.exe
if errorlevel 1 exit /b 1
copy target\release\daemon_pool_proxy.exe ..\app\src\main\resources\binaries\daemon_pool_proxy.exe
if errorlevel 1 exit /b 1
cd ..
cargo build --release
if errorlevel 1 exit /b 1
copy target\release\atomicswap.dll app\src\main\resources\atomicswap.dll
if errorlevel 1 exit /b 1

bash scripts/prepare/write-build-manifest.sh
if errorlevel 1 exit /b 1
