# ⚡ HUSHCORE | Local Real-Time Mic Processor

🌐 **[Türkçe Dokümantasyon için buraya tıklayın](README-TR.md)**

HushCore is a local, privacy-first, low-latency audio processing application designed to enhance microphone inputs in real-time. It runs entirely on your local machine, capturing input, applying dynamic DSP filters (Gate, EQ, Compressor, Limiter) and Noise Suppression (Dual-Mode: Eco DSP or HQ AI), and routing the output to a virtual audio cable for consumption in Discord, OBS, Zoom, and other voice clients.

---

## 🚀 Features

*   **100% Local & Private**: No audio streams are sent to the cloud. Everything processed frame-by-frame on your CPU.
*   **Dual-Mode Noise Suppression**:
    *   **Eco DSP**: A lightweight, self-contained spectral subtraction algorithm with zero external AI dependencies. Runs at extremely low CPU footprint.
    *   **HQ AI**: Incorporates **DeepFilterNet** deep learning models running on CPU for crystal-clear voice clarity.
*   **Studio FX Rack**:
    *   *Noise Gate*: Shuts down background static and key clicks when you are silent.
    *   *3-Band Parametric EQ*: Shape your voice warmth (Bass), clarity (Mids), and presence (Treble).
    *   *Dynamics Compressor*: Evens out vocal dynamics so you never sound too quiet or clip.
    *   *Peak Limiter*: Hard limiter protecting against digital clipping (>0 dBFS).
*   **Reflow-Free Level Meters**: Highly optimized web interface using GPU accelerated CSS transformations to achieve smooth 60fps rendering without UI lag.
*   **Routing Status Alert Banner**: Live UI warnings when outputting directly to physical speakers (feedback loops) vs routing successfully to Virtual Cable.

---

## 🛠️ Requirements

1.  **Node.js** (v18 or newer) - [Download](https://nodejs.org/)
2.  **Python** (3.8 - 3.12 recommended) - [Download](https://www.python.org/)
3.  **VB-Audio Virtual Cable** (Crucial to route audio to other apps) - [Download](https://vb-audio.com/Cable/)

---

## ⚙️ Installation & Run

### Windows (Quick Start)
Just double-click the **`run.bat`** file in the root folder. It will:
1.  Install frontend dependencies and compile static production assets.
2.  Create a Python Virtual Environment (`venv`) and install core dependencies.
3.  Prompt you if you wish to download the PyTorch AI components (optional, ~1.5 GB).
4.  Launch the local backend server.

---

## 🎧 Audio Routing Guide

To use HushCore as your microphone input in applications like Discord, Zoom, or OBS, you need to configure your audio routing correctly:

```
 [Physical Mic] ---> [HUSHCORE (Input)] 
                            |
                     (DSP & AI Processing)
                            |
                            v
                     [HUSHCORE (Output)] ---> [VB-Cable Input (Virtual)]
                                                        |
                                                 (Virtual Line)
                                                        |
                                                        v
                                              [VB-Cable Output] ---> [Discord/OBS/Zoom Input]
```

### 1. HushCore Dashboard Settings
1.  Open **`http://127.0.0.1:8000`** in your web browser.
2.  Select your physical microphone in **Microphone (Input Device)**.
3.  Select **`CABLE Input (VB-Audio Virtual Cable)`** in **Broadcast Output (Virtual Cable)**.
4.  Click the **`⚡ START ENGINE`** button.
5.  *Optional*: Enable **`Hear Myself (Monitor)`** and select your physical headphones if you want to listen to your processed audio in real-time.

> [!WARNING]
> **Why do I hear my own voice immediately?**
> If you select physical speakers or headphones as the **Broadcast Output**, the processed audio will play back to you. Always route the output to **VB-Cable Input** and keep "Hear Myself" disabled unless you explicitly want to monitor your voice.

### 2. Client Application Configuration
In Discord, OBS, or Zoom:
1.  Go to Settings -> Voice & Video.
2.  Set your **Input Device** to **`CABLE Output (VB-Audio Virtual Cable)`**.
3.  Set your **Output Device** to your physical headphones/speakers.

---

## 💻 Tech Stack

*   **Backend**: Python, FastAPI, WebSockets (Real-time meter & FFT streaming), Uvicorn.
*   **Audio DSP**: NumPy, SciPy Signal, SoundDevice (PortAudio wrapper).
*   **HQ AI Engine**: PyTorch, DeepFilterNet (Deep learning voice-enhancement).
*   **Frontend**: Vanilla HTML5/CSS3 (Glassmorphic neon theme), Javascript (ES modules), Vite build compiler.

---

## 📄 License

Distributed under the MIT License. See `LICENSE` for more information.
