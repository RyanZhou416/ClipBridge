@echo off
setlocal

REM === 1) 载入 VS/MSVC 环境（让 cl/link/lib/include 可用） ===
call "C:\Program Files\Microsoft Visual Studio\18\Community\Common7\Tools\VsDevCmd.bat" -arch=x64 -host_arch=x64
if errorlevel 1 (
  echo Failed to init VS DevCmd
  exit /b 1
)
set PATH=C:\Program Files\CMake\bin;%PATH%

REM === 2) aws-lc-sys 需要的环境 ===
set CMAKE_GENERATOR=Ninja
set AWS_LC_SYS_CFLAGS=/experimental:c11atomics
set AWS_LC_SYS_C_STD=11

REM === 3) 调用真正的 cargo build ===
REM 注意：这里用 `cargo.exe`，避免递归调用 alias
cargo.exe build %*

endlocal
