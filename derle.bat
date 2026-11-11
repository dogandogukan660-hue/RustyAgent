@echo off
chcp 65001 >nul
setlocal
cd /d "%~dp0"
title RustyAgent - derleme

echo ============================================================
echo  RustyAgent derleniyor
echo  Kutuphaneler internetten indirilir ve program derlenir.
echo  Ilk derleme 5-15 dakika surebilir.
echo ============================================================
echo.

where cargo >nul 2>nul
if errorlevel 1 (
    echo [!] Rust bulunamadi. Kurmak icin:
    echo     winget install Rustlang.Rustup
    echo     winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
    echo Kurduktan sonra bu dosyayi tekrar calistir.
    pause
    exit /b 1
)

cargo build --release
if errorlevel 1 (
    echo.
    echo [!] Derleme basarisiz. Yukaridaki hata mesajina bak.
    pause
    exit /b 1
)

if not exist dist mkdir dist
copy /y "target\release\RustyAgent.exe" "dist\RustyAgent.exe" >nul
copy /y "target\release\rusty-agent.exe" "dist\rusty-agent.exe" >nul
echo.
echo [OK] Program hazir: dist\RustyAgent.exe
echo.

choice /c EH /m "Derleme onbellegi (target klasoru, birkac GB) silinsin mi? Proje kucuk kalir, sonraki derleme biraz daha uzun surer"
if errorlevel 2 goto son
rmdir /s /q target
echo [OK] Onbellek silindi.

:son
echo.
pause
