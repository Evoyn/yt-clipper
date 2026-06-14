@echo off
REM Run cargo inside the MSVC + CUDA + libclang environment that whisper-rs needs.
REM Usage:  scripts\cargo-cuda.bat <cargo args>     e.g.  scripts\cargo-cuda.bat run -p yt-clipper
REM Discovers Visual Studio (vswhere), CUDA (CUDA_PATH), and LLVM automatically.
setlocal

REM --- Visual Studio vcvars (for cl.exe / link.exe / nvcc host compiler) ---
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if not exist "%VSWHERE%" (echo ERROR: vswhere not found -- install VS Build Tools & exit /b 1)
for /f "usebackq tokens=*" %%i in (`"%VSWHERE%" -latest -products * -property installationPath`) do set "VSPATH=%%i"
if not defined VSPATH (echo ERROR: no Visual Studio installation found & exit /b 1)
call "%VSPATH%\VC\Auxiliary\Build\vcvars64.bat" >nul

REM --- cargo on PATH ---
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

REM --- CUDA toolkit (nvcc) ---
if not defined CUDA_PATH if exist "%ProgramFiles%\NVIDIA GPU Computing Toolkit\CUDA\v13.3" set "CUDA_PATH=%ProgramFiles%\NVIDIA GPU Computing Toolkit\CUDA\v13.3"
if defined CUDA_PATH set "PATH=%CUDA_PATH%\bin;%PATH%"

REM --- libclang for bindgen (whisper-rs bundled bindings are Linux-only) ---
if not defined LIBCLANG_PATH if exist "%ProgramFiles%\LLVM\bin\libclang.dll" set "LIBCLANG_PATH=%ProgramFiles%\LLVM\bin"

REM --- limit the CUDA build to this GPU (RTX 3070 Ti = sm_86) unless overridden ---
if not defined CUDAARCHS set "CUDAARCHS=86"

cargo %*
exit /b %ERRORLEVEL%
