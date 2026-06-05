@echo off
setlocal enabledelayedexpansion

echo ===================================================
echo             HUSHCORE VOICE INITIALIZER
echo ===================================================

cd /d "%~dp0"

:: Check for Node.js
where node >nul 2>nul
if errorlevel 1 (
    echo [ERROR] Node.js is not installed or not in PATH.
    echo Please download and install Node.js from https://nodejs.org/
    echo.
    pause
    exit /b 1
)

:: Check for Python
where python >nul 2>nul
if errorlevel 1 (
    echo [ERROR] Python is not installed or not in PATH.
    echo Please install Python and ensure "Add Python to PATH" is checked during setup.
    echo.
    pause
    exit /b 1
)

:: Build frontend
echo.
echo [1/3] Building frontend assets...
if not exist "frontend" (
    echo [ERROR] 'frontend' folder not found in the current directory: %cd%
    pause
    exit /b 1
)

cd frontend
if not exist "node_modules" (
    echo node_modules not found, running npm install...
    call npm install
    if errorlevel 1 (
        echo [ERROR] npm install failed.
        pause
        exit /b 1
    )
)

echo Compiling Vite application...
call npm run build
if errorlevel 1 (
    echo [ERROR] Vite compilation failed.
    pause
    exit /b 1
)
cd ..

:: Setup Python Virtual Environment
echo.
echo [2/3] Preparing Python environment...
if not exist "venv" (
    echo Creating Python virtual environment - venv...
    python -m venv venv
    if errorlevel 1 (
        echo [ERROR] Failed to create virtual environment.
        pause
        exit /b 1
    )
)

echo Activating virtual environment...
if not exist "venv\Scripts\activate.bat" (
    echo [ERROR] Virtual environment activation script not found.
    pause
    exit /b 1
)
call venv\Scripts\activate.bat

echo Upgrading pip...
python -m pip install --upgrade pip

echo Installing core dependencies...
pip install -r backend\requirements.txt
if errorlevel 1 (
    echo [ERROR] Failed to install Python dependencies from backend\requirements.txt.
    pause
    exit /b 1
)

:: Ask user if they want to install PyTorch + DeepFilterNet
echo.
echo ===================================================
echo OPTIONAL: DeepFilterNet AI Engine Setup
echo ===================================================
echo Our app runs with a lightweight built-in Eco DSP Noise Suppression.
echo However, to enable the High-Quality AI engine (DeepFilterNet),
echo you can install PyTorch, torchaudio, and deepfilternet.
echo Note: This requires about 1.5GB of disk space.
echo.
set /p install_ai="Do you want to install the HQ AI components now? (y/n): "
if /i "%install_ai%"=="y" (
    echo Installing PyTorch CPU and torchaudio...
    pip install torch torchaudio --index-url https://download.pytorch.org/whl/cpu
    if errorlevel 1 (
        echo [WARNING] Failed to install PyTorch. Skipping AI component setup.
    ) else (
        echo Installing DeepFilterNet...
        pip install deepfilternet
        if errorlevel 1 (
            echo [WARNING] Failed to install DeepFilterNet. Skipping AI component setup.
        )
    )
) else (
    echo Skipping HQ AI components. Running in Eco DSP mode.
)

:: Launch App
echo.
echo [3/3] Starting HushCore backend server...
echo Access the dashboard at: http://127.0.0.1:8000
echo ===================================================
if not exist "backend\main.py" (
    echo [ERROR] backend\main.py not found!
    pause
    exit /b 1
)
python backend\main.py
if errorlevel 1 (
    echo.
    echo [ERROR] Backend server stopped unexpectedly or failed to start.
    pause
    exit /b 1
)

pause
