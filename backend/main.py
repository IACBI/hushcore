import os
import asyncio
import json
import uvicorn
import secrets
import threading
import webbrowser
from fastapi import FastAPI, WebSocket, WebSocketDisconnect, HTTPException, Request, Response
from fastapi.middleware.cors import CORSMiddleware
from fastapi.staticfiles import StaticFiles
from fastapi.responses import FileResponse, HTMLResponse
from pydantic import BaseModel

from config import load_settings, save_settings, DEFAULT_SETTINGS
from audio_processor import AudioProcessor

app = FastAPI(title="Voice Enhancement App API")

# Enable CORS for frontend development server
# Restrict CORS to local development and host endpoints to block malicious sites
app.add_middleware(
    CORSMiddleware,
    allow_origins=[
        "http://127.0.0.1:8000",
        "http://localhost:8000",
        "http://127.0.0.1:3000",
        "http://localhost:3000",
    ],
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)

# Global settings and audio processor
settings = load_settings()
processor = AudioProcessor(settings)

# Generate secure token
API_KEY = secrets.token_hex(16)

# API Authentication Middleware
@app.middleware("http")
async def auth_middleware(request: Request, call_next):
    if request.url.path.startswith("/api/"):
        api_key_header = request.headers.get("X-API-Key")
        if api_key_header != API_KEY:
            return Response(content="Unauthorized", status_code=401)
    response = await call_next(request)
    return response

# Audio pipeline will start stopped by default. User starts it from the dashboard.
print("Audio engine initialized in STOPPED state. Waiting for client to start.")

def validate_and_coerce(key: str, val):
    if key not in DEFAULT_SETTINGS:
        return None, False
    default_val = DEFAULT_SETTINGS[key]
    
    # Check if boolean
    if isinstance(default_val, bool):
        if isinstance(val, bool):
            return val, True
        if str(val).lower() in ["true", "1", "yes"]:
            return True, True
        if str(val).lower() in ["false", "0", "no"]:
            return False, True
        return None, False
        
    # Check if float
    elif isinstance(default_val, float):
        try:
            return float(val), True
        except (ValueError, TypeError):
            return None, False
            
    # Check if integer
    elif isinstance(default_val, int) and not isinstance(default_val, bool):
        try:
            return int(val), True
        except (ValueError, TypeError):
            return None, False
            
    # Check if string
    elif isinstance(default_val, str):
        return str(val), True
        
    return val, True

class SettingUpdate(BaseModel):
    key: str
    value: str | int | float | bool

@app.get("/api/devices")
def get_devices():
    try:
        return processor.get_devices()
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))

@app.get("/api/settings")
def get_settings():
    return settings

@app.post("/api/settings")
def update_setting(update: SettingUpdate):
    key = update.key
    val = update.value
    
    if key not in settings:
        raise HTTPException(status_code=400, detail=f"Invalid setting key: {key}")
        
    # Validate and coerce value to prevent real-time DSP thread TypeErrors
    coerced_val, valid = validate_and_coerce(key, val)
    if not valid:
        raise HTTPException(status_code=400, detail=f"Invalid value type for setting: {key}")
        
    settings[key] = coerced_val
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
        
    return {"status": "success", "settings": settings}

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

@app.websocket("/ws")
async def websocket_endpoint(websocket: WebSocket):
    token = websocket.query_params.get("token")
    if token != API_KEY:
        await websocket.close(code=4001)
        return
    await websocket.accept()
    
    async def send_metrics():
        while True:
            try:
                # Compile dynamic status metrics
                payload = {
                    "input_rms": processor.input_rms,
                    "output_rms": processor.output_rms,
                    "gate_state": processor.gate_state,
                    "fft_data": processor.fft_data,
                    "is_running": processor.running.is_set(),
                    "df_available": processor.df_available,
                    "vst_loaded": processor.vst_plugin is not None,
                    "vst_failed": processor.vst_failed
                }
                await websocket.send_text(json.dumps(payload))
                await asyncio.sleep(0.03)  # ~33 fps
            except Exception:
                break
                
    sender_task = asyncio.create_task(send_metrics())
    
    try:
        while True:
            data = await websocket.receive_text()
            msg = json.loads(data)
            
            if msg.get("action") == "update_setting":
                key = msg.get("key")
                val = msg.get("value")
                
                if key in settings:
                    coerced_val, valid = validate_and_coerce(key, val)
                    if valid:
                        settings[key] = coerced_val
                        save_settings(settings)
                        
                        # Update processor settings
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
    print("Warning: Frontend static assets directory not found. Please compile frontend with Vite.")

def get_injected_index():
    index_file = os.path.join(static_path, "index.html")
    if not os.path.exists(index_file):
        return "<h1>HushCore Frontend not compiled. Please run npm run build.</h1>"
    with open(index_file, "r", encoding="utf-8") as f:
        content = f.read()
    # Inject API key into head
    injection = f'<script>window.HUSHCORE_API_KEY = "{API_KEY}";</script>'
    content = content.replace("<head>", f"<head>{injection}")
    return content

@app.get("/")
def serve_home():
    return HTMLResponse(content=get_injected_index())

@app.get("/{fallback_path:path}")
def serve_spa_fallback(fallback_path: str):
    if fallback_path.startswith("assets/"):
        raise HTTPException(status_code=404, detail="Not Found")
    return HTMLResponse(content=get_injected_index())

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
        webbrowser.open("http://127.0.0.1:8000")
        
    def on_toggle_engine(icon, item):
        if processor.running.is_set():
            processor.stop()
        else:
            processor.start()
            
    def get_engine_label(item):
        return "🛑 Stop Audio Engine" if processor.running.is_set() else "⚡ Start Audio Engine"
        
    def on_exit(icon, item):
        print("Stopping HushCore server and exit...")
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
    
    print("\n===================================================")
    print("HUSHCORE BACKEND - SYSTEM TRAY INITIALIZER")
    print(f"API Access Key: {API_KEY}")
    print("Dashboard address: http://127.0.0.1:8000")
    print("===================================================\n")
    
    # Start web server in a daemon background thread
    server_thread = threading.Thread(
        target=lambda: uvicorn.run(app, host="127.0.0.1", port=8000, log_level="warning"),
        daemon=True
    )
    server_thread.start()
    
    # Start system tray icon in the main thread (blocks until exited)
    tray_icon = create_tray_icon()
    tray_icon.run()
