@echo off
rem HushCore launcher for Windows. It works when started from any folder.
rem
rem Usage: run.bat [options]
rem   --rebuild   Rebuild the dashboard even when backend\static already exists.
rem   --ai        Install the optional HQ AI components (PyTorch, DeepFilterNet) without asking.
rem   --no-ai     Never ask about the HQ AI components.
rem   --help      Show this help and exit.
rem
rem Without options: the dashboard is built only when backend\static is missing, venv is created
rem once, backend\requirements.txt is installed again only when it changes, and you are asked
rem once whether to install HQ AI when DeepFilterNet cannot be imported.
setlocal

set "FLAG_REBUILD="
set "FLAG_AI="
set "FLAG_NOAI="

:parse_args
if "%~1"=="" goto args_done
if /i "%~1"=="-h" goto usage
if /i "%~1"=="--help" goto usage
if /i "%~1"=="--rebuild" set "FLAG_REBUILD=1"
if /i "%~1"=="--ai" set "FLAG_AI=1"
if /i "%~1"=="--no-ai" set "FLAG_NOAI=1"
if /i not "%~1"=="--rebuild" if /i not "%~1"=="--ai" if /i not "%~1"=="--no-ai" goto bad_arg
shift
goto parse_args

:args_done
if defined FLAG_AI if defined FLAG_NOAI goto conflict_ai

cd /d "%~dp0"
set "VENV_PY=venv\Scripts\python.exe"

echo ===================================================
echo             HUSHCORE VOICE INITIALIZER
echo ===================================================

rem The backend uses "X | Y" annotations at runtime, which need Python 3.10 or newer.
python -c "import sys; sys.exit(0 if sys.version_info >= (3, 10) else 1)" >nul 2>nul
if errorlevel 1 goto no_python

echo.
echo [1/3] Dashboard
if defined FLAG_REBUILD goto build_frontend
if not exist "backend\static\index.html" goto build_frontend
echo Already built in backend\static. Use --rebuild to build it again.
goto frontend_done

:build_frontend
if not exist "frontend\package.json" goto no_frontend
where node >nul 2>nul
if errorlevel 1 goto no_node
where npm >nul 2>nul
if errorlevel 1 goto no_node
for /f "tokens=1 delims=." %%V in ('node -p "process.versions.node"') do set "NODE_MAJOR=%%V"
if not defined NODE_MAJOR goto no_node
if %NODE_MAJOR% LSS 20 goto no_node

pushd frontend
if exist "node_modules\" goto npm_ready
echo Installing frontend dependencies...
if exist "package-lock.json" goto npm_ci
call npm install
goto npm_installed
:npm_ci
call npm ci
:npm_installed
if errorlevel 1 goto npm_failed
:npm_ready
echo Compiling the dashboard with Vite...
call npm run build
if errorlevel 1 goto build_failed
popd

:frontend_done

echo.
echo [2/3] Python environment
if exist "%VENV_PY%" goto venv_ready
echo Creating virtual environment in venv...
python -m venv venv
if errorlevel 1 goto venv_failed
"%VENV_PY%" -m pip install --quiet --upgrade pip
if errorlevel 1 echo [WARNING] Could not upgrade pip. Continuing with the bundled version.

:venv_ready
rem The stamp is a copy of the requirements file kept inside venv, so pip runs only after a change.
fc /b "backend\requirements.txt" "venv\.deps-installed" >nul 2>nul
if not errorlevel 1 goto deps_ready
echo Installing core dependencies...
"%VENV_PY%" -m pip install --quiet -r "backend\requirements.txt"
if errorlevel 1 goto deps_failed
copy /y "backend\requirements.txt" "venv\.deps-installed" >nul
:deps_ready

echo.
echo HQ AI components (optional)
if defined FLAG_NOAI goto ai_done
if defined FLAG_AI goto ai_install
"%VENV_PY%" -c "import importlib.util, sys; sys.exit(0 if importlib.util.find_spec('df') else 1)" >nul 2>nul
if not errorlevel 1 goto ai_done
echo PyTorch and DeepFilterNet are not installed. Eco DSP noise suppression works without them.
echo The download is large. Pass --ai to install without this question, or --no-ai to skip it.
set "ANSWER="
set /p "ANSWER=Install the HQ AI components now? (y/N): "
if /i not "%ANSWER%"=="y" goto ai_done

:ai_install
echo Installing PyTorch (CPU build)...
"%VENV_PY%" -m pip install torch torchaudio --index-url https://download.pytorch.org/whl/cpu
if errorlevel 1 goto ai_failed
echo Installing DeepFilterNet...
"%VENV_PY%" -m pip install -r "backend\requirements-ai.txt"
if errorlevel 1 goto ai_failed
goto ai_done

:ai_failed
echo [WARNING] HQ AI install failed. Continuing with Eco DSP.

:ai_done

echo.
echo [3/3] Starting HushCore backend...
if not defined HUSHCORE_PORT set "HUSHCORE_PORT=8000"
echo Dashboard: http://127.0.0.1:%HUSHCORE_PORT% (set HUSHCORE_PORT to change the port)
echo ===================================================
"%VENV_PY%" backend\main.py
if errorlevel 1 goto server_failed
goto finished

:server_failed
echo.
echo [ERROR] The backend stopped with an error. Check the log above.
goto fail_pause

:finished
pause
endlocal
exit /b 0

:usage
echo Usage: run.bat [--rebuild] [--ai] [--no-ai] [--help]
echo.
echo   --rebuild   Rebuild the dashboard even when backend\static already exists.
echo   --ai        Install the optional HQ AI components without asking.
echo   --no-ai     Never ask about the HQ AI components.
echo   --help      Show this help and exit.
endlocal
exit /b 0

:bad_arg
echo [ERROR] Unknown option: %~1
echo Run "run.bat --help" to see the available options.
endlocal
exit /b 1

:conflict_ai
echo [ERROR] --ai and --no-ai cannot be used together.
endlocal
exit /b 1

:no_python
echo [ERROR] Python 3.10 or newer is required and must be on PATH.
echo Install it from https://www.python.org/ and tick "Add python.exe to PATH".
goto fail_pause

:no_frontend
echo [ERROR] The frontend folder was not found in %CD%.
goto fail_pause

:no_node
echo [ERROR] Node.js 20.19 or newer is required to build the dashboard.
echo Install it from https://nodejs.org/ (it is only needed to build the dashboard).
goto fail_pause

:npm_failed
echo [ERROR] npm install failed.
goto fail_pause

:build_failed
echo [ERROR] Vite build failed.
goto fail_pause

:venv_failed
echo [ERROR] Could not create the virtual environment in venv.
goto fail_pause

:deps_failed
echo [ERROR] Installing backend\requirements.txt failed.
goto fail_pause

:fail_pause
echo.
pause
endlocal
exit /b 1
