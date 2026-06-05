import time
import queue
import threading
import numpy as np
import scipy.signal as signal
import sounddevice as sd
import sys

try:
    from pedalboard import VST3Plugin
except ImportError:
    VST3Plugin = None

class SpectralSubtractedNoiseSuppression:
    def __init__(self, block_size=480, hop_size=240, sr=48000):
        self.block_size = block_size
        self.hop_size = hop_size
        self.sr = sr
        
        self.input_buffer = np.zeros(block_size, dtype=np.float32)
        self.output_buffer = np.zeros(block_size, dtype=np.float32)
        self.window = np.hanning(block_size).astype(np.float32)
        
        self.noise_profile = np.zeros(block_size // 2 + 1, dtype=np.float32)
        self.noise_initialized = False
        self.alpha = 0.95
        self.beta = 2.0
        self.floor = 0.05
        
    def process(self, chunk, strength=0.8):
        self.input_buffer[:-self.hop_size] = self.input_buffer[self.hop_size:]
        self.input_buffer[-self.hop_size:] = chunk
        
        win_input = self.input_buffer * self.window
        
        fft_val = np.fft.rfft(win_input)
        mag = np.abs(fft_val).astype(np.float32)
        rms = np.sqrt(np.mean(chunk**2) + 1e-12)
        
        if not self.noise_initialized:
            self.noise_profile = mag.copy()
            self.noise_initialized = True
        elif rms < 0.005:
            self.noise_profile = self.alpha * self.noise_profile + (1.0 - self.alpha) * mag
        else:
            self.noise_profile = 0.999 * self.noise_profile + 0.001 * mag
            
        sub_mag = mag - (self.beta * strength) * self.noise_profile
        sub_mag = np.maximum(sub_mag, self.floor * mag)
        
        # Reconstruct phase factor algebraically to avoid expensive trigonometric np.angle and np.exp
        phase_factor = fft_val / (mag + 1e-12)
        clean_fft = sub_mag * phase_factor
        clean_win = np.fft.irfft(clean_fft, n=self.block_size).astype(np.float32)
        
        self.output_buffer += clean_win * self.window
        
        out_chunk = self.output_buffer[:self.hop_size].copy()
        
        self.output_buffer[:-self.hop_size] = self.output_buffer[self.hop_size:]
        self.output_buffer[-self.hop_size:] = 0.0
        
        return out_chunk


class Equalizer3Band:
    def __init__(self, sr=48000):
        self.sr = sr
        self.low_gain = 0.0
        self.mid_gain = 0.0
        self.high_gain = 0.0
        
        self.update_filters()
        self.reset_state()
        
    def reset_state(self):
        self.zi_low = np.zeros(2, dtype=np.float32)
        self.zi_mid = np.zeros(2, dtype=np.float32)
        self.zi_high = np.zeros(2, dtype=np.float32)
        
    def update_gains(self, low, mid, high):
        if low != self.low_gain or mid != self.mid_gain or high != self.high_gain:
            self.low_gain = low
            self.mid_gain = mid
            self.high_gain = high
            self.update_filters()
            
    def update_filters(self):
        self.b_low, self.a_low = self.design_low_shelf(200.0, 0.707, self.low_gain)
        self.b_mid, self.a_mid = self.design_peaking(1000.0, 0.707, self.mid_gain)
        self.b_high, self.a_high = self.design_high_shelf(4000.0, 0.707, self.high_gain)
        
    def design_low_shelf(self, f0, Q, gain_db):
        A = 10.0 ** (gain_db / 40.0)
        w0 = 2.0 * np.pi * f0 / self.sr
        cos_w0 = np.cos(w0)
        alpha = np.sin(w0) / 2.0 * np.sqrt((A + 1.0/A)*(1.0/Q - 1.0) + 2.0)
        
        b0 = A * ((A + 1.0) - (A - 1.0)*cos_w0 + 2.0*np.sqrt(A)*alpha)
        b1 = 2.0 * A * ((A - 1.0) - (A + 1.0)*cos_w0)
        b2 = A * ((A + 1.0) - (A - 1.0)*cos_w0 - 2.0*np.sqrt(A)*alpha)
        
        a0 = (A + 1.0) + (A - 1.0)*cos_w0 + 2.0*np.sqrt(A)*alpha
        a1 = -2.0 * ((A - 1.0) + (A + 1.0)*cos_w0)
        a2 = (A + 1.0) + (A - 1.0)*cos_w0 - 2.0*np.sqrt(A)*alpha
        
        return np.array([b0, b1, b2], dtype=np.float32) / a0, np.array([a0, a1, a2], dtype=np.float32) / a0

    def design_peaking(self, f0, Q, gain_db):
        A = 10.0 ** (gain_db / 40.0)
        w0 = 2.0 * np.pi * f0 / self.sr
        alpha = np.sin(w0) / (2.0 * Q)
        cos_w0 = np.cos(w0)
        
        b0 = 1.0 + alpha * A
        b1 = -2.0 * cos_w0
        b2 = 1.0 - alpha * A
        
        a0 = 1.0 + alpha / A
        a1 = -2.0 * cos_w0
        a2 = 1.0 - alpha / A
        
        return np.array([b0, b1, b2], dtype=np.float32) / a0, np.array([a0, a1, a2], dtype=np.float32) / a0

    def design_high_shelf(self, f0, Q, gain_db):
        A = 10.0 ** (gain_db / 40.0)
        w0 = 2.0 * np.pi * f0 / self.sr
        cos_w0 = np.cos(w0)
        alpha = np.sin(w0) / 2.0 * np.sqrt((A + 1.0/A)*(1.0/Q - 1.0) + 2.0)
        
        b0 = A * ((A + 1.0) + (A - 1.0)*cos_w0 + 2.0*np.sqrt(A)*alpha)
        b1 = -2.0 * A * ((A - 1.0) + (A + 1.0)*cos_w0)
        b2 = A * ((A + 1.0) - (A - 1.0)*cos_w0 - 2.0*np.sqrt(A)*alpha)
        
        a0 = (A + 1.0) - (A - 1.0)*cos_w0 + 2.0*np.sqrt(A)*alpha
        a1 = 2.0 * ((A - 1.0) - (A + 1.0)*cos_w0)
        a2 = (A + 1.0) - (A - 1.0)*cos_w0 - 2.0*np.sqrt(A)*alpha
        
        return np.array([b0, b1, b2], dtype=np.float32) / a0, np.array([a0, a1, a2], dtype=np.float32) / a0

    def process(self, data):
        y, self.zi_low = signal.lfilter(self.b_low, self.a_low, data, zi=self.zi_low)
        y, self.zi_mid = signal.lfilter(self.b_mid, self.a_mid, y, zi=self.zi_mid)
        y, self.zi_high = signal.lfilter(self.b_high, self.a_high, y, zi=self.zi_high)
        return y.astype(np.float32)


class DynamicsCompressor:
    def __init__(self, sr=48000):
        self.sr = sr
        self.threshold_db = -18.0
        self.ratio = 3.0
        self.attack_ms = 10.0
        self.release_ms = 100.0
        self.env_db = -60.0
        
    def process(self, data):
        rms = np.sqrt(np.mean(data**2) + 1e-12)
        rms_db = 20.0 * np.log10(rms)
        
        attack_alpha = np.exp(-10.0 / self.attack_ms)
        release_alpha = np.exp(-10.0 / self.release_ms)
        
        if rms_db > self.env_db:
            self.env_db = attack_alpha * self.env_db + (1.0 - attack_alpha) * rms_db
        else:
            self.env_db = release_alpha * self.env_db + (1.0 - release_alpha) * rms_db
            
        gain_db = 0.0
        if self.env_db > self.threshold_db:
            excess_db = self.env_db - self.threshold_db
            gain_db = excess_db * (1.0 / self.ratio - 1.0)
            
        gain_linear = 10.0 ** (gain_db / 20.0)
        return data * gain_linear


class VocalExciter:
    def __init__(self, sr=48000):
        self.sr = sr
        self.freq = 3000.0
        self.amount = 0.2
        self.mix = 0.15
        self.update_filters()
        self.reset_state()
        
    def reset_state(self):
        self.zi = np.zeros(2, dtype=np.float32)
        
    def update_params(self, freq, amount, mix):
        if freq != self.freq:
            self.freq = freq
            self.update_filters()
        self.amount = amount
        self.mix = mix
        
    def update_filters(self):
        # Design high pass filter at self.freq
        w0 = 2.0 * np.pi * self.freq / self.sr
        cos_w0 = np.cos(w0)
        alpha = np.sin(w0) / (2.0 * 0.707)  # Q = 0.707
        
        b0 = (1.0 + cos_w0) / 2.0
        b1 = -(1.0 + cos_w0)
        b2 = (1.0 + cos_w0) / 2.0
        a0 = 1.0 + alpha
        a1 = -2.0 * cos_w0
        a2 = 1.0 - alpha
        
        self.b = np.array([b0, b1, b2], dtype=np.float32) / a0
        self.a = np.array([a0, a1, a2], dtype=np.float32) / a0
        
    def process(self, data):
        # High pass filter
        hp_data, self.zi = signal.lfilter(self.b, self.a, data, zi=self.zi)
        # Generate harmonics
        excited = np.tanh(hp_data * (1.0 + self.amount * 5.0))
        # Mix back
        out = data * (1.0 - self.mix) + excited * self.mix
        return out.astype(np.float32)


class VocalDeEsser:
    def __init__(self, sr=48000):
        self.sr = sr
        self.threshold_db = -25.0
        self.amount = 0.5
        self.freq = 6000.0  # Center sibilant frequency
        self.update_filters()
        self.reset_state()
        
    def reset_state(self):
        self.zi_bp = np.zeros(2, dtype=np.float32)
        self.zi_hs = np.zeros(2, dtype=np.float32)
        self.env = 0.0
        
    def update_params(self, threshold_db, amount):
        self.threshold_db = threshold_db
        self.amount = amount
        
    def update_filters(self):
        # Band-pass filter centered at 6kHz (sibilant band)
        w0 = 2.0 * np.pi * self.freq / self.sr
        cos_w0 = np.cos(w0)
        alpha = np.sin(w0) / (2.0 * 1.0)  # Q = 1.0
        
        b0 = alpha
        b1 = 0.0
        b2 = -alpha
        a0 = 1.0 + alpha
        a1 = -2.0 * cos_w0
        a2 = 1.0 - alpha
        
        self.b_bp = np.array([b0, b1, b2], dtype=np.float32) / a0
        self.a_bp = np.array([a0, a1, a2], dtype=np.float32) / a0
        
    def process(self, data):
        # Isolate sibilant energy
        sib_data, self.zi_bp = signal.lfilter(self.b_bp, self.a_bp, data, zi=self.zi_bp)
        
        # Compute envelope of sibilant energy
        sib_rms = np.sqrt(np.mean(sib_data**2) + 1e-12)
        
        # Smooth envelope
        alpha_env = np.exp(-10.0 / 15.0)  # 15ms release time
        self.env = alpha_env * self.env + (1.0 - alpha_env) * sib_rms
        env_db = 20.0 * np.log10(self.env + 1e-12)
        
        gain_reduction_db = 0.0
        if env_db > self.threshold_db:
            excess_db = env_db - self.threshold_db
            gain_reduction_db = -excess_db * self.amount
            gain_reduction_db = max(-15.0, gain_reduction_db)
            
        # Design high shelf filter starting at 5kHz for dynamic attenuation
        f0 = 5000.0
        A = 10.0 ** (gain_reduction_db / 40.0)
        w0 = 2.0 * np.pi * f0 / self.sr
        cos_w0 = np.cos(w0)
        alpha = np.sin(w0) / 2.0 * np.sqrt((A + 1.0/A)*(1.0/0.707 - 1.0) + 2.0)
        
        b0 = A * ((A + 1.0) + (A - 1.0)*cos_w0 + 2.0*np.sqrt(A)*alpha)
        b1 = -2.0 * A * ((A - 1.0) + (A + 1.0)*cos_w0)
        b2 = A * ((A + 1.0) - (A - 1.0)*cos_w0 - 2.0*np.sqrt(A)*alpha)
        
        a0 = (A + 1.0) - (A - 1.0)*cos_w0 + 2.0*np.sqrt(A)*alpha
        a1 = 2.0 * ((A - 1.0) - (A + 1.0)*cos_w0)
        a2 = (A + 1.0) - (A - 1.0)*cos_w0 - 2.0*np.sqrt(A)*alpha
        
        b_hs = np.array([b0, b1, b2], dtype=np.float32) / a0
        a_hs = np.array([a0, a1, a2], dtype=np.float32) / a0
        
        out, self.zi_hs = signal.lfilter(b_hs, a_hs, data, zi=self.zi_hs)
        return out.astype(np.float32)


class AudioProcessor:
    def __init__(self, settings):
        self.settings = settings
        self.sr = 48000
        self.block_size = 480  # 10ms frame size
        
        # Initialize DSP modules
        self.eco_ns = SpectralSubtractedNoiseSuppression(sr=self.sr)
        self.eq = Equalizer3Band(sr=self.sr)
        self.compressor = DynamicsCompressor(sr=self.sr)
        self.deesser = VocalDeEsser(sr=self.sr)
        self.exciter = VocalExciter(sr=self.sr)
        self.vst_plugin = None
        self.vst_failed = False
        self._loaded_vst_path = ""
        
        # DeepFilterNet integration
        self.df_available = False
        self.df_model = None
        self.df_state = None
        self._init_deep_filter()
        
        # Audio status metrics
        self.input_rms = 0.0
        self.output_rms = 0.0
        self.gate_state = False
        self.fft_data = [0.0] * 64
        
        # Pipeline State
        self.gate_gain = 0.0
        self.limiter_decay = 0.999
        self.limiter_gain = 1.0
        
        # Queue Buffers
        self.input_queue = queue.Queue(maxsize=100)
        self.output_queue = queue.Queue(maxsize=100)
        self.monitor_queue = queue.Queue(maxsize=100)
        
        # Audio Streams
        self.input_stream = None
        self.output_stream = None
        self.monitor_stream = None
        
        # Worker control
        self.running = threading.Event()
        self.processor_thread = None
        
        # Accumulators
        self.input_accumulator = np.array([], dtype=np.float32)
        self.output_accumulator = np.array([], dtype=np.float32)
        self.monitor_accumulator = np.array([], dtype=np.float32)

    def _init_deep_filter(self):
        try:
            print("Attempting to load DeepFilterNet...")
            from df.enhance import init_df, enhance
            import torch
            self.df_model, self.df_state, _ = init_df()
            self.df_enhance_fn = enhance
            self.torch_module = torch
            self.df_available = True
            print("DeepFilterNet loaded successfully!")
        except Exception as e:
            self.df_model = None
            self.df_state = None
            self.df_enhance_fn = None
            self.torch_module = None
            self.df_available = False
            print(f"DeepFilterNet not available (using Eco Mode fallback): {e}")

    def update_settings(self, settings):
        self.settings = settings
        
        # Update EQ
        if self.settings["eq_enabled"]:
            self.eq.update_gains(
                self.settings["eq_low_gain_db"],
                self.settings["eq_mid_gain_db"],
                self.settings["eq_high_gain_db"]
            )
            
        # Update Compressor
        self.compressor.threshold_db = self.settings["compressor_threshold_db"]
        self.compressor.ratio = self.settings["compressor_ratio"]
        self.compressor.attack_ms = max(1.0, self.settings["compressor_attack_ms"])
        self.compressor.release_ms = max(10.0, self.settings["compressor_release_ms"])
        
        # Update De-Esser
        self.deesser.update_params(
            self.settings.get("deesser_threshold_db", -25.0),
            self.settings.get("deesser_amount", 0.5)
        )
        
        # Update Vocal Exciter
        self.exciter.update_params(
            self.settings.get("exciter_frequency", 3000.0),
            self.settings.get("exciter_amount", 0.2),
            self.settings.get("exciter_mix", 0.15)
        )
        
        # Update VST3 Plugin
        vst_enabled = self.settings.get("vst_enabled", False)
        vst_path = self.settings.get("vst_path", "")
        
        if not vst_enabled or not vst_path:
            if self.vst_plugin is not None:
                print("Unloading VST3 plugin...")
                self.vst_plugin = None
            self.vst_failed = False
        else:
            if self.vst_plugin is None or self._loaded_vst_path != vst_path:
                print(f"Loading VST3 plugin: {vst_path}")
                try:
                    if VST3Plugin is not None:
                        self.vst_plugin = VST3Plugin(vst_path)
                        self._loaded_vst_path = vst_path
                        self.vst_failed = False
                        print(f"Successfully loaded VST3: {self.vst_plugin.name}")
                    else:
                        raise ImportError("pedalboard is not installed or VST3Plugin could not be imported.")
                except Exception as e:
                    print(f"Error loading VST3 plugin: {e}", file=sys.stderr)
                    self.vst_plugin = None
                    self.vst_failed = True

    def get_devices(self):
        devices = sd.query_devices()
        hostapis = sd.query_hostapis()
        input_devs = []
        output_devs = []
        
        for idx, dev in enumerate(devices):
            api_name = hostapis[dev["hostapi"]]["name"]
            dev_info = {
                "id": idx,
                "name": f"{dev['name']} [{api_name}]",
                "raw_name": dev["name"],
                "hostapi": api_name,
                "max_input_channels": dev["max_input_channels"],
                "max_output_channels": dev["max_output_channels"],
                "default_samplerate": dev["default_samplerate"]
            }
            if dev["max_input_channels"] > 0:
                input_devs.append(dev_info)
            if dev["max_output_channels"] > 0:
                output_devs.append(dev_info)
                
        return {"inputs": input_devs, "outputs": output_devs}

    def _input_callback(self, indata, frames, time, status):
        if status:
            print(f"Input stream status: {status}", file=sys.stderr)
        
        data_mono = np.mean(indata, axis=1).astype(np.float32)
        self.input_accumulator = np.append(self.input_accumulator, data_mono)
        
        while len(self.input_accumulator) >= self.block_size:
            frame = self.input_accumulator[:self.block_size]
            self.input_accumulator = self.input_accumulator[self.block_size:]
            
            try:
                self.input_queue.put_nowait(frame)
            except queue.Full:
                try:
                    self.input_queue.get_nowait()
                    self.input_queue.put_nowait(frame)
                except queue.Empty:
                    pass

    def _output_callback(self, outdata, frames, time, status):
        if status:
            print(f"Output stream status: {status}", file=sys.stderr)
            
        while len(self.output_accumulator) < frames:
            try:
                block = self.output_queue.get_nowait()
                self.output_accumulator = np.append(self.output_accumulator, block)
            except queue.Empty:
                needed = frames - len(self.output_accumulator)
                self.output_accumulator = np.append(self.output_accumulator, np.zeros(needed, dtype=np.float32))
                break
                
        data = self.output_accumulator[:frames]
        self.output_accumulator = self.output_accumulator[frames:]
        
        if outdata.shape[1] == 2:
            outdata[:, 0] = data
            outdata[:, 1] = data
        else:
            outdata[:, 0] = data

    def _monitor_callback(self, outdata, frames, time, status):
        if status:
            print(f"Monitor stream status: {status}", file=sys.stderr)
            
        while len(self.monitor_accumulator) < frames:
            try:
                block = self.monitor_queue.get_nowait()
                self.monitor_accumulator = np.append(self.monitor_accumulator, block)
            except queue.Empty:
                needed = frames - len(self.monitor_accumulator)
                self.monitor_accumulator = np.append(self.monitor_accumulator, np.zeros(needed, dtype=np.float32))
                break
                
        data = self.monitor_accumulator[:frames]
        self.monitor_accumulator = self.monitor_accumulator[frames:]
        
        if outdata.shape[1] == 2:
            outdata[:, 0] = data
            outdata[:, 1] = data
        else:
            outdata[:, 0] = data

    def start(self):
        if self.running.is_set():
            return True
        
        self.running.set()
        
        # Clear queues
        while not self.input_queue.empty(): self.input_queue.get()
        while not self.output_queue.empty(): self.output_queue.get()
        while not self.monitor_queue.empty(): self.monitor_queue.get()
        
        self.input_accumulator = np.array([], dtype=np.float32)
        self.output_accumulator = np.array([], dtype=np.float32)
        self.monitor_accumulator = np.array([], dtype=np.float32)
        
        # Open audio devices
        input_idx = self._find_device_idx(self.settings["input_device"], is_input=True)
        output_idx = self._find_device_idx(self.settings["output_device"], is_input=False)
        monitor_idx = self._find_device_idx(self.settings["monitor_device"], is_input=False)
        
        # Negotiate sample rate and frame block sizes before initializing streams
        self.sr = self._negotiate_sample_rate(input_idx, output_idx, monitor_idx)
        self.block_size = int(self.sr * 0.01)  # 10ms frame size
        
        # Re-initialize DSP modules with the negotiated sample rate
        self.eco_ns = SpectralSubtractedNoiseSuppression(
            block_size=self.block_size, 
            hop_size=self.block_size // 2, 
            sr=self.sr
        )
        self.eq = Equalizer3Band(sr=self.sr)
        self.compressor = DynamicsCompressor(sr=self.sr)
        self.deesser = VocalDeEsser(sr=self.sr)
        self.exciter = VocalExciter(sr=self.sr)
        self.update_settings(self.settings)
        
        # Start processing thread
        self.processor_thread = threading.Thread(target=self._processing_loop, daemon=True)
        self.processor_thread.start()
        
        # Determine stream block size for manual latency tuning
        stream_blocksize = 0
        buf_size_setting = self.settings.get("buffer_size", "auto")
        if buf_size_setting != "auto":
            try:
                stream_blocksize = int(buf_size_setting)
            except ValueError:
                pass

        try:
            self.input_stream = sd.InputStream(
                device=input_idx,
                samplerate=self.sr,
                channels=1,
                blocksize=stream_blocksize,
                callback=self._input_callback
            )
            self.input_stream.start()
            print(f"Started input stream on device {input_idx} (blocksize={stream_blocksize})")
        except Exception as e:
            print(f"Failed to start input stream: {e}", file=sys.stderr)
            self.stop()
            return False

        try:
            self.output_stream = sd.OutputStream(
                device=output_idx,
                samplerate=self.sr,
                channels=2,
                blocksize=stream_blocksize,
                callback=self._output_callback
            )
            self.output_stream.start()
            print(f"Started output stream on device {output_idx}")
        except Exception as e:
            print(f"Failed to start output stream: {e}", file=sys.stderr)
            self.stop()
            return False
            
        if self.settings["monitor_enabled"] and monitor_idx is not None:
            self.start_monitoring(self.settings["monitor_device"])
            
        return True

    def start_monitoring(self, device_name):
        self.stop_monitoring()
        monitor_idx = self._find_device_idx(device_name, is_input=False)
        if monitor_idx is None:
            return
            
        stream_blocksize = 0
        buf_size_setting = self.settings.get("buffer_size", "auto")
        if buf_size_setting != "auto":
            try:
                stream_blocksize = int(buf_size_setting)
            except ValueError:
                pass

        try:
            self.monitor_stream = sd.OutputStream(
                device=monitor_idx,
                samplerate=self.sr,
                channels=2,
                blocksize=stream_blocksize,
                callback=self._monitor_callback
            )
            self.monitor_stream.start()
            print(f"Started monitoring stream on device {monitor_idx}")
        except Exception as e:
            print(f"Failed to start monitoring: {e}", file=sys.stderr)

    def stop_monitoring(self):
        if self.monitor_stream is not None:
            try:
                self.monitor_stream.stop()
                self.monitor_stream.close()
            except Exception:
                pass
            self.monitor_stream = None
            print("Stopped monitoring stream.")

    def stop(self):
        self.running.clear()
        
        if self.input_stream is not None:
            try: self.input_stream.stop(); self.input_stream.close()
            except Exception: pass
            self.input_stream = None
            
        if self.output_stream is not None:
            try: self.output_stream.stop(); self.output_stream.close()
            except Exception: pass
            self.output_stream = None
            
        self.stop_monitoring()
        
        if self.processor_thread is not None:
            self.processor_thread.join(timeout=1.0)
            self.processor_thread = None
            
        print("Audio pipeline stopped.")

    def _find_device_idx(self, device_name, is_input=True):
        devices = sd.query_devices()
        hostapis = sd.query_hostapis()
        
        if device_name:
            # 1. Match full name with Host API suffix
            for idx, dev in enumerate(devices):
                api_name = hostapis[dev["hostapi"]]["name"]
                full_name = f"{dev['name']} [{api_name}]"
                if full_name == device_name:
                    if is_input and dev["max_input_channels"] > 0:
                        return idx
                    if not is_input and dev["max_output_channels"] > 0:
                        return idx
            
            # 2. Substring match
            for idx, dev in enumerate(devices):
                if device_name.lower() in dev["name"].lower():
                    if is_input and dev["max_input_channels"] > 0:
                        return idx
                    if not is_input and dev["max_output_channels"] > 0:
                        return idx
        else:
            # If no device selected on startup, search for a Virtual Cable to avoid playing to speakers
            if not is_input:
                for idx, dev in enumerate(devices):
                    api_name = hostapis[dev["hostapi"]]["name"]
                    dev_name_lower = dev["name"].lower()
                    if ("cable" in dev_name_lower or "virtual" in dev_name_lower or "vb-audio" in dev_name_lower) and dev["max_output_channels"] > 0:
                        print(f"Auto-selected Virtual Cable for default output: {dev['name']} [{api_name}]")
                        return idx
                        
        # 3. Fallback: Find the default WASAPI device
        for idx, dev in enumerate(devices):
            api_name = hostapis[dev["hostapi"]]["name"]
            if "wasapi" in api_name.lower():
                if is_input and dev["max_input_channels"] > 0:
                    return idx
                if not is_input and dev["max_output_channels"] > 0:
                    return idx
                    
        # 4. Final Fallback
        default = sd.default.device[0] if is_input else sd.default.device[1]
        return default

    def _negotiate_sample_rate(self, input_idx, output_idx, monitor_idx):
        rates_to_try = [48000, 44100]
        
        # Gather default samplerates of the devices as fallbacks
        devs = sd.query_devices()
        fallback_rates = []
        for idx in [input_idx, output_idx, monitor_idx]:
            if idx is not None and idx < len(devs):
                rate = int(devs[idx]["default_samplerate"])
                if rate not in rates_to_try and rate not in fallback_rates:
                    fallback_rates.append(rate)
        rates_to_try.extend(fallback_rates)
        
        for rate in rates_to_try:
            try:
                # check_input_settings/check_output_settings check PortAudio compatibility
                sd.check_input_settings(device=input_idx, samplerate=rate, channels=1)
                sd.check_output_settings(device=output_idx, samplerate=rate, channels=2)
                if self.settings["monitor_enabled"] and monitor_idx is not None:
                    sd.check_output_settings(device=monitor_idx, samplerate=rate, channels=2)
                
                print(f"Sample rate negotiated: {rate} Hz")
                return rate
            except Exception:
                continue
                
        # Last resort fallback
        if input_idx is not None and input_idx < len(devs):
            return int(devs[input_idx]["default_samplerate"])
        return 48000

    def _processing_loop(self):
        print("Audio processing thread started.")
        while self.running.is_set():
            try:
                frame = self.input_queue.get(timeout=0.1)
            except queue.Empty:
                continue
                
            in_rms = np.sqrt(np.mean(frame**2) + 1e-12)
            self.input_rms = 20.0 * np.log10(in_rms)
            
            frame = frame * self.settings["input_gain"]
            
            if self.settings["gate_enabled"]:
                gate_db = self.settings["gate_threshold_db"]
                gate_release = self.settings["gate_release_ms"]
                
                curr_rms = np.sqrt(np.mean(frame**2) + 1e-12)
                curr_db = 20.0 * np.log10(curr_rms)
                
                attack_alpha = np.exp(-10.0 / 2.0)
                release_alpha = np.exp(-10.0 / gate_release)
                
                if curr_db > gate_db:
                    target_gain = 1.0
                    self.gate_state = True
                    alpha = attack_alpha
                else:
                    target_gain = 0.0
                    self.gate_state = False
                    alpha = release_alpha
                    
                self.gate_gain = alpha * self.gate_gain + (1.0 - alpha) * target_gain
                frame = frame * self.gate_gain
            else:
                self.gate_state = True
                self.gate_gain = 1.0
                
            ns_mode = self.settings["ns_mode"]
            hop = self.block_size // 2
            
            # DeepFilterNet is strictly designed for 48kHz. Fallback to Eco DSP at other sample rates.
            if ns_mode == "high" and self.df_available and self.sr == 48000:
                try:
                    frame_t = self.torch_module.from_numpy(frame).float().unsqueeze(0)
                    with self.torch_module.no_grad():
                        enhanced_t = self.df_enhance_fn(self.df_model, self.df_state, frame_t)
                    frame = enhanced_t.squeeze(0).numpy().copy()
                except Exception as e:
                    print(f"DeepFilterNet frame processing error, falling back: {e}", file=sys.stderr)
                    frame_half1 = self.eco_ns.process(frame[:hop], strength=self.settings["ns_eco_strength"])
                    frame_half2 = self.eco_ns.process(frame[hop:], strength=self.settings["ns_eco_strength"])
                    frame = np.concatenate([frame_half1, frame_half2])
            elif ns_mode in ["high", "eco"]: # If HQ AI is selected but sample rate != 48kHz, run Eco DSP
                frame_half1 = self.eco_ns.process(frame[:hop], strength=self.settings["ns_eco_strength"])
                frame_half2 = self.eco_ns.process(frame[hop:], strength=self.settings["ns_eco_strength"])
                frame = np.concatenate([frame_half1, frame_half2])
                
            # De-Esser: early placement to soften sibilance before EQ/comp
            if self.settings.get("deesser_enabled", False):
                frame = self.deesser.process(frame)
                
            if self.settings["eq_enabled"]:
                frame = self.eq.process(frame)
                
            if self.settings["compressor_enabled"]:
                frame = self.compressor.process(frame)
                
            # Vocal Exciter: late placement before Peak Limiter
            if self.settings.get("exciter_enabled", False):
                frame = self.exciter.process(frame)
                
            # VST3 Hosting: processed right before Peak Limiter
            if self.settings.get("vst_enabled", False) and self.vst_plugin is not None:
                try:
                    # Reshape to 2D (channels, samples) for pedalboard VST
                    audio_2d = frame.reshape(1, -1)
                    processed_2d = self.vst_plugin(audio_2d, self.sr)
                    frame = processed_2d.flatten()
                except Exception:
                    pass
                
            lim_thresh = 10.0 ** (self.settings["limiter_threshold_db"] / 20.0)
            peak = np.max(np.abs(frame))
            
            if peak > lim_thresh:
                target_lim_gain = lim_thresh / peak
                frame = frame * target_lim_gain
                self.limiter_gain = target_lim_gain
            else:
                self.limiter_gain = self.limiter_decay * self.limiter_gain + (1.0 - self.limiter_decay) * 1.0
                frame = frame * self.limiter_gain
                
            frame = frame * self.settings["output_gain"]
            frame = np.clip(frame, -1.0, 1.0)
            
            out_rms = np.sqrt(np.mean(frame**2) + 1e-12)
            self.output_rms = 20.0 * np.log10(out_rms)
            
            spec = np.abs(np.fft.rfft(frame))[:64]
            spec_norm = np.clip(20.0 * np.log10(spec + 1e-5) + 60.0, 0.0, 60.0) / 60.0
            self.fft_data = spec_norm.tolist()
            
            try:
                self.output_queue.put_nowait(frame)
            except queue.Full:
                try:
                    self.output_queue.get_nowait()
                    self.output_queue.put_nowait(frame)
                except queue.Empty:
                    pass
                    
            if self.settings["monitor_enabled"] and self.monitor_stream is not None:
                try:
                    self.monitor_queue.put_nowait(frame)
                except queue.Full:
                    try:
                        self.monitor_queue.get_nowait()
                        self.monitor_queue.put_nowait(frame)
                    except queue.Empty:
                        pass
                        
        print("Audio processing thread finished.")
