import os
import asyncio
import json
import logging
import secrets
import threading
import time
import urllib.request
import webbrowser
import uvicorn
from fastapi import FastAPI, WebSocket, WebSocketDisconnect, HTTPException, Request, Response
from fastapi.middleware.cors import CORSMiddleware
from fastapi.staticfiles import StaticFiles
from fastapi.responses import HTMLResponse, PlainTextResponse
from pydantic import BaseModel

from config import load_settings, save_settings, validate_and_coerce
from audio_processor import AudioProcessor
from presets import (
    BUILTIN_PRESETS,
    delete_user_preset,
    find_preset,
    get_user_presets,
    sanitize_preset_name,
    save_user_preset,
    snapshot_dsp,
)

logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s: %(message)s")
logger = logging.getLogger("hushcore")

APP_VERSION = "1.1.0"
PUBLIC_API_PATHS = frozenset({"/api/health"})

def _resolve_port():
    raw = os.environ.get("HUSHCORE_PORT", "").strip()
    if not raw:
        return 8000
    try:
        port = int(raw)
    except ValueError:
        port = 0
    if not 1 <= port <= 65535:
        logger.warning("Invalid HUSHCORE_PORT %r, falling back to 8000", raw)
        return 8000
    return port

PORT = _resolve_port()
DASHBOARD_URL = f"http://127.0.0.1:{PORT}"

app = FastAPI(title="Voice Enhancement App API")

# Tests extend ALLOWED_HOSTS to admit the TestClient's "testserver" host.
ALLOWED_HOSTS = {f"127.0.0.1:{PORT}", f"localhost:{PORT}"}
ALLOWED_ORIGINS = {
    f"http://127.0.0.1:{PORT}",
    f"http://localhost:{PORT}",
    "http://127.0.0.1:3000",
    "http://localhost:3000",
}
AUTH_TIMEOUT_S = 5.0

def _first_header(scope, name):
    for key, value in scope["headers"]:
        if key == name:
            return value.decode("latin-1").lower()
    return None

class HostOriginGuard:
    """Rejects foreign Host headers (DNS rebinding) and foreign Origins (cross-site WebSocket)."""

    def __init__(self, asgi_app):
        self.app = asgi_app

    async def __call__(self, scope, receive, send):
        if scope["type"] not in ("http", "websocket"):
            await self.app(scope, receive, send)
            return
        host = _first_header(scope, b"host")
        origin = _first_header(scope, b"origin")
        if host not in ALLOWED_HOSTS or (origin is not None and origin not in ALLOWED_ORIGINS):
            if scope["type"] == "websocket":
                await send({"type": "websocket.close", "code": 1008})
            else:
                await PlainTextResponse("Forbidden", status_code=403)(scope, receive, send)
            return
        await self.app(scope, receive, send)

# Enable CORS for frontend development server
# Restrict CORS to local development and host endpoints to block malicious sites
app.add_middleware(
    CORSMiddleware,
    allow_origins=sorted(ALLOWED_ORIGINS),
    allow_credentials=True,
    allow_methods=["GET", "POST", "DELETE"],
    allow_headers=["Content-Type", "X-API-Key"],
)
app.add_middleware(HostOriginGuard)

# Generate secure token
API_KEY = secrets.token_hex(16)

def _api_key_matches(provided):
    if not provided:
        return False
    return secrets.compare_digest(provided.encode("utf-8"), API_KEY.encode("utf-8"))

# API Authentication Middleware
@app.middleware("http")
async def auth_middleware(request: Request, call_next):
    path = request.url.path
    if path.startswith("/api/") and path not in PUBLIC_API_PATHS:
        if not _api_key_matches(request.headers.get("X-API-Key")):
            return Response(content="Unauthorized", status_code=401)
    response = await call_next(request)
    return response

# Global settings and audio processor
settings = load_settings()
processor = AudioProcessor(settings)

# Serializes mutations of settings: REST, WebSocket and preset calls run on
# different threads and the DSP thread reads the same dict.
settings_lock = threading.RLock()

# Audio pipeline will start stopped by default. User starts it from the dashboard.
logger.info("Audio engine initialized in STOPPED state. Waiting for client to start.")

class SettingUpdate(BaseModel):
    key: str
    value: str | int | float | bool

class PresetRequest(BaseModel):
    name: str

def apply_setting(key, value, *, persist=True):
    """Validate, store and push one setting to the processor. Returns (ok, error)."""
    with settings_lock:
        coerced_val, error = validate_and_coerce(key, value)
        if error is not None:
            return False, error

        settings[key] = coerced_val
        if persist:
            save_settings(settings)

        # Process device restarts or toggle monitoring
        if key in ["input_device", "output_device"]:
            processor.stop()
            processor.update_settings(settings)
            processor.start()
        elif key == "monitor_enabled":
            processor.settings[key] = coerced_val
            if coerced_val:
                processor.start_monitoring(settings["monitor_device"])
            else:
                processor.stop_monitoring()
        elif key == "monitor_device":
            processor.settings[key] = coerced_val
            if settings["monitor_enabled"]:
                processor.start_monitoring(coerced_val)
        else:
            processor.update_settings(settings)
    return True, None

def _settings_snapshot():
    with settings_lock:
        return dict(settings)

@app.get("/api/health")
def health():
    return {"status": "ok", "version": APP_VERSION}

@app.get("/api/devices")
def get_devices():
    try:
        return processor.get_devices()
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))

@app.get("/api/settings")
def get_settings():
    return _settings_snapshot()

@app.post("/api/settings")
def update_setting(update: SettingUpdate):
    ok, error = apply_setting(update.key, update.value)
    if not ok:
        raise HTTPException(status_code=400, detail=error)
    return {"status": "success", "settings": _settings_snapshot()}

@app.get("/api/presets")
def list_presets():
    return {"builtin": BUILTIN_PRESETS, "user": get_user_presets()}

@app.post("/api/presets/apply")
def apply_preset(body: PresetRequest):
    values = find_preset(body.name)
    if values is None:
        raise HTTPException(status_code=404, detail="Preset not found")

    with settings_lock:
        for key, value in values.items():
            ok, error = apply_setting(key, value, persist=False)
            if not ok:
                logger.warning("Preset value rejected: %s", error)
        save_settings(settings)
    return {"status": "success", "settings": _settings_snapshot()}

@app.post("/api/presets")
def save_preset(body: PresetRequest):
    try:
        name = sanitize_preset_name(body.name)
    except ValueError as e:
        raise HTTPException(status_code=400, detail=str(e))

    with settings_lock:
        current = snapshot_dsp(settings)
    try:
        saved = save_user_preset(name, current)
    except OSError as e:
        logger.error("Could not save user preset: %s", e)
        raise HTTPException(status_code=500, detail="Could not write presets file")
    return {"status": "success", "name": name, "preset": saved}

@app.delete("/api/presets/{name}")
def remove_preset(name: str):
    if not delete_user_preset(name):
        raise HTTPException(status_code=404, detail="Preset not found")
    return {"status": "success"}

@app.post("/api/vst/show-editor")
def show_vst_editor():
    if processor.vst_plugin is not None:
        try:
            processor.vst_plugin.show_editor()
            return {"status": "success", "message": "VST editor opened"}
        except Exception as e:
            raise HTTPException(status_code=500, detail=f"Failed to open VST editor: {e}")
    raise HTTPException(status_code=400, detail="No VST3 plugin loaded")

@app.post("/api/start")
def start_pipeline():
    success = processor.start()
    if not success:
        raise HTTPException(status_code=500, detail="Failed to start audio pipeline")
    return {"status": "started"}

@app.post("/api/stop")
def stop_pipeline():
    processor.stop()
    return {"status": "stopped"}

def _json_default(obj):
    # numpy scalars and arrays from the DSP side
    if hasattr(obj, "tolist"):
        return obj.tolist()
    raise TypeError(f"Object of type {type(obj).__name__} is not JSON serializable")

def build_metrics():
    get_status = getattr(processor, "get_status", None)
    if callable(get_status):
        return get_status()
    return {
        "input_rms": processor.input_rms,
        "output_rms": processor.output_rms,
        "gate_state": processor.gate_state,
        "fft_data": processor.fft_data,
        "is_running": processor.running.is_set(),
        "df_available": processor.df_available,
        "vst_loaded": processor.vst_plugin is not None,
        "vst_failed": processor.vst_failed
    }

async def _handle_ws_message(raw):
    try:
        msg = json.loads(raw)
    except (TypeError, ValueError):
        logger.warning("Ignoring malformed WebSocket message")
        return
    if not isinstance(msg, dict) or msg.get("action") != "update_setting":
        return

    key = msg.get("key")
    try:
        ok, error = await asyncio.to_thread(apply_setting, key, msg.get("value"))
    except Exception:
        logger.exception("Failed to apply WebSocket setting %r", key)
        return
    if not ok:
        logger.warning("Rejected WebSocket setting: %s", error)

async def _ws_authenticated(websocket: WebSocket) -> bool:
    try:
        message = await asyncio.wait_for(websocket.receive(), timeout=AUTH_TIMEOUT_S)
    except asyncio.TimeoutError:
        return False
    text = message.get("text")
    if text is None:
        return False
    try:
        msg = json.loads(text)
    except ValueError:
        return False
    if not isinstance(msg, dict) or msg.get("action") != "auth":
        return False
    token = msg.get("token")
    return isinstance(token, str) and _api_key_matches(token)

@app.websocket("/ws")
async def websocket_endpoint(websocket: WebSocket):
    # Accept first: browsers only expose an application close code such as 4001 once the
    # handshake has completed. Refusing before accept surfaces as a bare HTTP 403.
    await websocket.accept()
    if not await _ws_authenticated(websocket):
        await websocket.close(code=4001)
        return
    await websocket.send_text(json.dumps({"action": "auth_ok"}))

    async def send_metrics():
        while True:
            try:
                await websocket.send_text(json.dumps(build_metrics(), default=_json_default))
            except Exception:
                break
            await asyncio.sleep(0.03)  # ~33 fps

    sender_task = asyncio.create_task(send_metrics())

    try:
        while True:
            message = await websocket.receive()
            if message["type"] == "websocket.disconnect":
                break
            text = message.get("text")
            if text is not None:
                await _handle_ws_message(text)
    except WebSocketDisconnect:
        pass
    finally:
        sender_task.cancel()

# Serve frontend static assets (CSS, JS) if compiled
static_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "static")
assets_path = os.path.join(static_path, "assets")
if os.path.exists(assets_path):
    app.mount("/assets", StaticFiles(directory=assets_path), name="assets")
else:
    logger.warning("Frontend static assets directory not found. Please compile frontend with Vite.")

def get_injected_index():
    index_file = os.path.join(static_path, "index.html")
    if not os.path.exists(index_file):
        return "<h1>HushCore Frontend not compiled. Please run npm run build.</h1>"
    with open(index_file, "r", encoding="utf-8") as f:
        content = f.read()
    # json.dumps keeps the value a valid JS string; escaping "<" stops the
    # value from closing the script tag early.
    key_literal = json.dumps(API_KEY).replace("<", "\\u003c")
    injection = f'<script>window.HUSHCORE_API_KEY = {key_literal};</script>'
    content = content.replace("<head>", f"<head>{injection}")
    return content

NO_STORE_HEADERS = {"Cache-Control": "no-store"}

@app.get("/")
def serve_home():
    return HTMLResponse(content=get_injected_index(), headers=NO_STORE_HEADERS)

@app.get("/{fallback_path:path}")
def serve_spa_fallback(fallback_path: str):
    if fallback_path.startswith("assets/") or fallback_path.startswith("api/"):
        raise HTTPException(status_code=404, detail="Not Found")
    return HTMLResponse(content=get_injected_index(), headers=NO_STORE_HEADERS)

def _open_dashboard_when_ready(timeout=15.0):
    # Proxy-free opener: a system proxy must not intercept the local health check
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with opener.open(f"{DASHBOARD_URL}/api/health", timeout=1) as resp:
                if resp.status == 200:
                    break
        except OSError:
            time.sleep(0.25)
    webbrowser.open(DASHBOARD_URL)

def create_tray_icon():
    import pystray
    from PIL import Image, ImageDraw

    def create_icon_image():
        img = Image.new("RGBA", (64, 64), (8, 8, 12, 255))
        draw = ImageDraw.Draw(img)
        # Draw outer neon cyan circle
        draw.ellipse([8, 8, 56, 56], outline=(0, 242, 254, 255), width=4)
        # Draw neon pink 'H'
        draw.rectangle([22, 18, 26, 46], fill=(255, 0, 127, 255))
        draw.rectangle([38, 18, 42, 46], fill=(255, 0, 127, 255))
        draw.rectangle([22, 30, 42, 34], fill=(255, 0, 127, 255))
        return img

    def on_open_dashboard(icon, item):
        webbrowser.open(DASHBOARD_URL)

    def on_toggle_engine(icon, item):
        if processor.running.is_set():
            processor.stop()
        else:
            processor.start()

    def get_engine_label(item):
        return "🛑 Stop Audio Engine" if processor.running.is_set() else "⚡ Start Audio Engine"

    def on_exit(icon, item):
        logger.info("Stopping HushCore server and exiting")
        processor.stop()
        icon.stop()
        os._exit(0)

    menu = pystray.Menu(
        pystray.MenuItem("🌐 Open Dashboard", on_open_dashboard),
        pystray.MenuItem(get_engine_label, on_toggle_engine),
        pystray.MenuItem("Exit", on_exit)
    )

    icon = pystray.Icon("HushCore", create_icon_image(), "HushCore Voice Processor", menu)
    return icon

if __name__ == "__main__":
    # Ensure static directory exists
    os.makedirs(static_path, exist_ok=True)

    logger.info("HushCore backend - system tray initializer")
    logger.info("Dashboard address: %s", DASHBOARD_URL)

    # Start web server in a daemon background thread
    server_thread = threading.Thread(
        target=lambda: uvicorn.run(app, host="127.0.0.1", port=PORT, log_level="warning"),
        daemon=True
    )
    server_thread.start()

    if os.environ.get("HUSHCORE_NO_BROWSER") != "1":
        threading.Thread(target=_open_dashboard_when_ready, daemon=True).start()

    # Start system tray icon in the main thread (blocks until exited)
    tray_icon = create_tray_icon()
    tray_icon.run()
