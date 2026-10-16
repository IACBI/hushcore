#!/usr/bin/env bash
# HushCore launcher for macOS and Linux. It works when started from any folder.
#
# Usage: ./run.sh [options]
#   --rebuild   Rebuild the dashboard and the native engine even when they already exist.
#   --ai        Install the optional HQ AI components (PyTorch, DeepFilterNet) without asking.
#   --no-ai     Never ask about the HQ AI components.
#   --help      Show this help and exit.
#
# Without options: the dashboard is built only when backend/static is missing, venv is created
# once, backend/requirements.txt is installed again only when it changes, and you are asked once
# whether to install HQ AI when DeepFilterNet cannot be imported (only in an interactive shell).
# The native audio engine is built with cargo only when no engine binary exists yet, or with
# --rebuild. Without Rust the Python engine is used.
set -euo pipefail

ENGINE_NAME="hushcore-engine"
VENV_PY="venv/bin/python"

FLAG_REBUILD=0
FLAG_AI=0
FLAG_NOAI=0

usage() {
    cat <<'EOF'
Usage: ./run.sh [--rebuild] [--ai] [--no-ai] [--help]

  --rebuild   Rebuild the dashboard and the native engine even when they already exist.
  --ai        Install the optional HQ AI components (PyTorch, DeepFilterNet) without asking.
  --no-ai     Never ask about the HQ AI components.
  --help      Show this help and exit.
EOF
}

fail() {
    echo "[ERROR] $1" >&2
    exit 1
}

for arg in "$@"; do
    case "$arg" in
        -h|--help) usage; exit 0 ;;
        --rebuild) FLAG_REBUILD=1 ;;
        --ai) FLAG_AI=1 ;;
        --no-ai) FLAG_NOAI=1 ;;
        *)
            echo "[ERROR] Unknown option: $arg" >&2
            echo 'Run "./run.sh --help" to see the available options.' >&2
            exit 1
            ;;
    esac
done

if [ "$FLAG_AI" = 1 ] && [ "$FLAG_NOAI" = 1 ]; then
    fail "--ai and --no-ai cannot be used together."
fi

cd "$(dirname "$0")"

build_dashboard() {
    [ -f frontend/package.json ] || fail "The frontend folder was not found in $(pwd)."
    if ! command -v node >/dev/null 2>&1 || ! command -v npm >/dev/null 2>&1; then
        fail "Node.js 20.19 or newer is required to build the dashboard. Install it from https://nodejs.org/ (it is only needed to build the dashboard)."
    fi
    local node_major
    node_major=$(node -p "process.versions.node.split('.')[0]")
    if [ "$node_major" -lt 20 ]; then
        fail "Node.js 20.19 or newer is required to build the dashboard. Install it from https://nodejs.org/ (it is only needed to build the dashboard)."
    fi

    cd frontend
    if [ ! -d node_modules ]; then
        echo "Installing frontend dependencies..."
        if [ -f package-lock.json ]; then
            npm ci || fail "npm install failed."
        else
            npm install || fail "npm install failed."
        fi
    fi
    echo "Compiling the dashboard with Vite..."
    npm run build || fail "Vite build failed."
    cd ..
}

install_ai_components() {
    echo "Installing PyTorch..."
    if [ "$(uname -s)" = "Linux" ]; then
        # The CPU index avoids the large CUDA build. macOS wheels are CPU builds already.
        "$VENV_PY" -m pip install torch torchaudio --index-url https://download.pytorch.org/whl/cpu || return 1
    else
        "$VENV_PY" -m pip install torch torchaudio || return 1
    fi
    echo "Installing DeepFilterNet..."
    "$VENV_PY" -m pip install -r backend/requirements-ai.txt || return 1
}

engine_present() {
    [ -f "backend/bin/$ENGINE_NAME" ] || [ -f "engine/target/release/$ENGINE_NAME" ]
}

echo "==================================================="
echo "            HUSHCORE VOICE INITIALIZER"
echo "==================================================="

# The backend uses "X | Y" annotations at runtime, which need Python 3.10 or newer.
if ! command -v python3 >/dev/null 2>&1 || ! python3 -c "import sys; sys.exit(0 if sys.version_info >= (3, 10) else 1)" >/dev/null 2>&1; then
    fail "Python 3.10 or newer is required and must be available as python3. On macOS use Homebrew (brew install python) or https://www.python.org/; on Debian or Ubuntu use sudo apt install python3 python3-venv."
fi

echo
echo "[1/3] Dashboard"
if [ "$FLAG_REBUILD" = 1 ] || [ ! -f backend/static/index.html ]; then
    build_dashboard
else
    echo "Already built in backend/static. Use --rebuild to build it again."
fi

echo
echo "[2/3] Python environment"
if [ ! -x "$VENV_PY" ]; then
    echo "Creating virtual environment in venv..."
    python3 -m venv venv || fail "Could not create the virtual environment in venv. On Debian or Ubuntu install python3-venv."
    "$VENV_PY" -m pip install --quiet --upgrade pip || echo "[WARNING] Could not upgrade pip. Continuing with the bundled version."
fi

# The stamp is a copy of the requirements file kept inside venv, so pip runs only after a change.
if ! cmp -s backend/requirements.txt venv/.deps-installed 2>/dev/null; then
    echo "Installing core dependencies..."
    "$VENV_PY" -m pip install --quiet -r backend/requirements.txt || fail "Installing backend/requirements.txt failed."
    cp backend/requirements.txt venv/.deps-installed
fi

if ! "$VENV_PY" -c "import sounddevice" >/dev/null 2>&1; then
    echo "[WARNING] sounddevice cannot load PortAudio, which the Python engine needs."
    echo "          On Linux install it with: sudo apt install libportaudio2"
fi

echo
echo "HQ AI components (optional)"
if [ "$FLAG_NOAI" = 0 ]; then
    install_ai=0
    if [ "$FLAG_AI" = 1 ]; then
        install_ai=1
    elif ! "$VENV_PY" -c "import importlib.util, sys; sys.exit(0 if importlib.util.find_spec('df') else 1)" >/dev/null 2>&1; then
        echo "PyTorch and DeepFilterNet are not installed. Eco DSP noise suppression works without them."
        echo "The download is large. Pass --ai to install without this question, or --no-ai to skip it."
        if [ -t 0 ]; then
            answer=""
            read -r -p "Install the HQ AI components now? (y/N): " answer || true
            case "$answer" in
                y|Y) install_ai=1 ;;
            esac
        fi
    fi
    if [ "$install_ai" = 1 ]; then
        install_ai_components || echo "[WARNING] HQ AI install failed. Continuing with Eco DSP."
    fi
fi

echo
echo "Native audio engine (optional)"
if [ "$FLAG_REBUILD" = 1 ] || ! engine_present; then
    if command -v cargo >/dev/null 2>&1; then
        echo "Building the native engine with cargo (release)..."
        cargo build --release --manifest-path engine/Cargo.toml || echo "[WARNING] Native engine build failed. HushCore will use the Python engine."
    elif ! engine_present; then
        echo "Rust (cargo) was not found, so the Python engine will be used."
        echo "Install Rust from https://rustup.rs/ to enable the native engine. This is optional."
    fi
fi

echo
echo "[3/3] Starting HushCore backend..."
export HUSHCORE_PORT="${HUSHCORE_PORT:-8000}"
echo "Dashboard: http://127.0.0.1:${HUSHCORE_PORT} (set HUSHCORE_PORT to change the port)"
echo "==================================================="
if ! "$VENV_PY" backend/main.py; then
    fail "The backend stopped with an error. Check the log above."
fi
