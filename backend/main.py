import os
import asyncio
import json
import uvicorn
from fastapi import FastAPI, WebSocket, WebSocketDisconnect, HTTPException
from fastapi.middleware.cors import CORSMiddleware
from fastapi.staticfiles import StaticFiles
from fastapi.responses import FileResponse
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
                    "df_available": processor.df_available
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

# Serve frontend build statically if it exists
static_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "static")
if os.path.exists(static_path):
    app.mount("/", StaticFiles(directory=static_path, html=True), name="static")
else:
    print("Warning: Frontend static directory not found. Please compile frontend with Vite.")

@app.get("/{fallback_path:path}")
def serve_spa_fallback(fallback_path: str):
    index_file = os.path.join(static_path, "index.html")
    if os.path.exists(index_file):
        return FileResponse(index_file)
    raise HTTPException(status_code=404, detail="Not Found")

if __name__ == "__main__":
    # Ensure static directory exists to prevent startup crash if mounted later
    os.makedirs(static_path, exist_ok=True)
    # Start web server
    uvicorn.run(app, host="127.0.0.1", port=8000)
