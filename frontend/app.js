/* ==========================================================================
   HUSHCORE - CLIENT APPLICATION LOGIC
   ========================================================================== */

const API_BASE = window.location.origin;
const WS_URL = `ws://${window.location.host}/ws`;

let socket = null;
let settings = {};
let devices = { inputs: [], outputs: [] };

// Visualizer State
let currentVizMode = 'spec';
let rawFFTData = new Array(64).fill(0);
let smoothedFFT = new Array(64).fill(0);
let inputLevelSmoothed = -60;
let outputLevelSmoothed = -60;
let isGateOpen = false;

// DOM Cache
const dom = {
    appWrapper: document.getElementById('app-wrapper'),
    
    // View Mode selectors
    viewModeSimple: document.getElementById('view-mode-simple'),
    viewModeAdv: document.getElementById('view-mode-adv'),
    
    // Header controls
    powerBtn: document.getElementById('power-btn'),
    engineStatus: document.getElementById('engine-status'),
    aiStatus: document.getElementById('ai-status'),
    connIndicator: document.getElementById('connection-indicator'),
    refreshDevicesBtn: document.getElementById('refresh-devices-btn'),
    
    // Routing panel
    inputSelect: document.getElementById('input-device-select'),
    outputSelect: document.getElementById('output-device-select'),
    routingTipBox: document.getElementById('routing-tip-box'),
    monitorCheckbox: document.getElementById('monitor-enabled-checkbox'),
    monitorSelect: document.getElementById('monitor-device-select'),
    
    // Level Meters & Gains
    inputGainSlider: document.getElementById('input-gain-slider'),
    inputGainVal: document.getElementById('input-gain-val'),
    outputGainSlider: document.getElementById('output-gain-slider'),
    outputGainVal: document.getElementById('output-gain-val'),
    inputMeterFill: document.getElementById('input-meter-fill'),
    gateMarker: document.getElementById('gate-threshold-marker'),
    outputMeterFill: document.getElementById('output-meter-fill'),
    
    // Canvas Visualizer
    canvas: document.getElementById('visualizer-canvas'),
    vizTabSpec: document.getElementById('viz-tab-spec'),
    vizTabWave: document.getElementById('viz-tab-wave'),
    gateLed: document.getElementById('gate-led'),
    gateText: document.getElementById('gate-text'),
    
    // Easy mode presets
    presetOff: document.getElementById('preset-btn-off'),
    presetVoice: document.getElementById('preset-btn-voice'),
    presetGaming: document.getElementById('preset-btn-gaming'),
    presetBroadcaster: document.getElementById('preset-btn-broadcaster'),
    easyNSSlider: document.getElementById('easy-ns-slider'),
    easyNSVal: document.getElementById('easy-ns-val'),
    easyToneSlider: document.getElementById('easy-tone-slider'),
    easyToneVal: document.getElementById('easy-tone-val'),
    
    // Advanced NS
    nsModeOff: document.getElementById('ns-mode-off'),
    nsModeEco: document.getElementById('ns-mode-eco'),
    nsModeHigh: document.getElementById('ns-mode-high'),
    dfInfo: document.getElementById('df-availability-info'),
    nsStrengthSlider: document.getElementById('ns-strength-slider'),
    nsStrengthVal: document.getElementById('ns-strength-val'),
    
    // Advanced Noise Gate
    gateCheckbox: document.getElementById('gate-enabled-checkbox'),
    gateThreshSlider: document.getElementById('gate-thresh-slider'),
    gateThreshVal: document.getElementById('gate-thresh-val'),
    gateReleaseSlider: document.getElementById('gate-release-slider'),
    gateReleaseVal: document.getElementById('gate-release-val'),
    
    // Advanced EQ
    eqCheckbox: document.getElementById('eq-enabled-checkbox'),
    eqLowSlider: document.getElementById('eq-low-slider'),
    eqLowVal: document.getElementById('eq-low-val'),
    eqMidSlider: document.getElementById('eq-mid-slider'),
    eqMidVal: document.getElementById('eq-mid-val'),
    eqHighSlider: document.getElementById('eq-high-slider'),
    eqHighVal: document.getElementById('eq-high-val'),
    
    // Advanced Compressor
    compCheckbox: document.getElementById('compressor-enabled-checkbox'),
    compThreshSlider: document.getElementById('comp-thresh-slider'),
    compThreshVal: document.getElementById('comp-thresh-val'),
    compRatioSlider: document.getElementById('comp-ratio-slider'),
    compRatioVal: document.getElementById('comp-ratio-val'),
    compAttackSlider: document.getElementById('comp-attack-slider'),
    compAttackVal: document.getElementById('comp-attack-val'),
    compReleaseSlider: document.getElementById('comp-release-slider'),
    compReleaseVal: document.getElementById('comp-release-val'),

    // Advanced De-Esser
    deesserCheckbox: document.getElementById('deesser-enabled-checkbox'),
    deesserThreshSlider: document.getElementById('deesser-thresh-slider'),
    deesserThreshVal: document.getElementById('deesser-thresh-val'),
    deesserAmountSlider: document.getElementById('deesser-amount-slider'),
    deesserAmountVal: document.getElementById('deesser-amount-val'),

    // Advanced Vocal Exciter
    exciterCheckbox: document.getElementById('exciter-enabled-checkbox'),
    exciterFreqSlider: document.getElementById('exciter-freq-slider'),
    exciterFreqVal: document.getElementById('exciter-freq-val'),
    exciterAmountSlider: document.getElementById('exciter-amount-slider'),
    exciterAmountVal: document.getElementById('exciter-amount-val'),
    exciterMixSlider: document.getElementById('exciter-mix-slider'),
    exciterMixVal: document.getElementById('exciter-mix-val')
};

// Canvas context setup
const ctx = dom.canvas.getContext('2d');
resizeCanvas();
window.addEventListener('resize', resizeCanvas);

function resizeCanvas() {
    const dpr = window.devicePixelRatio || 1;
    const rect = dom.canvas.getBoundingClientRect();
    dom.canvas.width = rect.width * dpr;
    dom.canvas.height = rect.height * dpr;
    ctx.scale(dpr, dpr);
}

// --- Initialize App ---
async function init() {
    try {
        await fetchSettings();
        await fetchDevices();
        setupEventListeners();
        connectWebSocket();
        startVisualizationLoop();
    } catch (err) {
        console.error("Initialization error:", err);
    }
}

// --- API Services ---
async function fetchSettings() {
    const res = await fetch(`${API_BASE}/api/settings`);
    settings = await res.json();
    applySettingsToUI(settings);
    detectActivePreset(settings);
}

async function fetchDevices() {
    const res = await fetch(`${API_BASE}/api/devices`);
    devices = await res.json();
    populateDevices(devices);
}

function populateDevices(devList) {
    // Microphone dropdown
    dom.inputSelect.innerHTML = '';
    devList.inputs.forEach(d => {
        const opt = document.createElement('option');
        opt.value = d.name;
        opt.textContent = d.name;
        dom.inputSelect.appendChild(opt);
    });
    if (settings.input_device) dom.inputSelect.value = settings.input_device;
    
    // VB-Cable dropdown
    dom.outputSelect.innerHTML = '';
    devList.outputs.forEach(d => {
        const opt = document.createElement('option');
        opt.value = d.name;
        opt.textContent = d.name;
        dom.outputSelect.appendChild(opt);
    });
    if (settings.output_device) dom.outputSelect.value = settings.output_device;

    // Monitor select dropdown
    dom.monitorSelect.innerHTML = '<option value="">Kulaklık Seçin...</option>';
    devList.outputs.forEach(d => {
        const opt = document.createElement('option');
        opt.value = d.name;
        opt.textContent = d.name;
        dom.monitorSelect.appendChild(opt);
    });
    if (settings.monitor_device) dom.monitorSelect.value = settings.monitor_device;
    
    updateRoutingWarning();
}

// Inspect output device selection and show warning
function updateRoutingWarning() {
    const selectedOutput = dom.outputSelect.value;
    if (!selectedOutput) {
        dom.routingTipBox.textContent = "Çıkış aygıtı seçilmedi.";
        dom.routingTipBox.className = "warning-banner warning";
        return;
    }
    
    const outputLower = selectedOutput.toLowerCase();
    
    // Check if output is a physical playback device (speakers, headphones, Realtek, etc.)
    const isPhysical = outputLower.includes('speaker') || 
                       outputLower.includes('hoparlör') || 
                       outputLower.includes('headphone') || 
                       outputLower.includes('kulaklık') || 
                       outputLower.includes('realtek') || 
                       outputLower.includes('high definition') || 
                       (outputLower.includes('audio device') && !outputLower.includes('cable') && !outputLower.includes('virtual'));
                       
    if (isPhysical) {
        dom.routingTipBox.innerHTML = `⚠️ <strong>Uyarı: Hoparlör Çıkışı Seçildi!</strong><br>
Kendi sesinizi duyarsınız. Discord, Zoom veya OBS ile kullanmak için çıkışı <strong>VB-Cable</strong> gibi sanal bir kabloya yönlendirin.`;
        dom.routingTipBox.className = "warning-banner warning";
    } else {
        dom.routingTipBox.innerHTML = `✅ <strong>Sanal Kablo Aktif</strong><br>
Ses sanal hatta gönderiliyor. Discord/OBS ses girişi ayarlarından <strong>VB-Cable Output</strong> seçerek kullanabilirsiniz.`;
        dom.routingTipBox.className = "warning-banner success";
    }
}

function updateGateMarkerPosition(threshDb) {
    if (!dom.gateMarker) return;
    if (!settings.gate_enabled) {
        dom.gateMarker.style.display = 'none';
        return;
    }
    dom.gateMarker.style.display = 'block';
    const pct = dbToPercent(threshDb);
    dom.gateMarker.style.left = `${pct}%`;
}

// Update the UI fields with current settings values
function applySettingsToUI(cfg) {
    // Monitor Checkbox & Device Select state
    dom.monitorCheckbox.checked = cfg.monitor_enabled;
    dom.monitorSelect.disabled = !cfg.monitor_enabled;
    
    // Gain values & labels
    dom.inputGainSlider.value = cfg.input_gain;
    dom.inputGainVal.textContent = `${cfg.input_gain.toFixed(2)}x`;
    dom.outputGainSlider.value = cfg.output_gain;
    dom.outputGainVal.textContent = `${cfg.output_gain.toFixed(2)}x`;
    
    // Noise Suppression (Advanced & Easy)
    updateNSModeButtons(cfg.ns_mode);
    dom.nsStrengthSlider.value = Math.round(cfg.ns_eco_strength * 100);
    dom.nsStrengthVal.textContent = `${Math.round(cfg.ns_eco_strength * 100)}%`;
    dom.easyNSSlider.value = Math.round(cfg.ns_eco_strength * 100);
    dom.easyNSVal.textContent = `${Math.round(cfg.ns_eco_strength * 100)}%`;
    
    // Noise Gate
    dom.gateCheckbox.checked = cfg.gate_enabled;
    dom.gateThreshSlider.value = cfg.gate_threshold_db;
    dom.gateThreshVal.textContent = `${cfg.gate_threshold_db} dB`;
    dom.gateReleaseSlider.value = cfg.gate_release_ms;
    dom.gateReleaseVal.textContent = `${cfg.gate_release_ms} ms`;
    updateGateMarkerPosition(cfg.gate_threshold_db);
    
    // Equalizer
    dom.eqCheckbox.checked = cfg.eq_enabled;
    dom.eqLowSlider.value = cfg.eq_low_gain_db;
    dom.eqLowVal.textContent = `${cfg.eq_low_gain_db > 0 ? '+' : ''}${cfg.eq_low_gain_db.toFixed(1)} dB`;
    dom.eqMidSlider.value = cfg.eq_mid_gain_db;
    dom.eqMidVal.textContent = `${cfg.eq_mid_gain_db > 0 ? '+' : ''}${cfg.eq_mid_gain_db.toFixed(1)} dB`;
    dom.eqHighSlider.value = cfg.eq_high_gain_db;
    dom.eqHighVal.textContent = `${cfg.eq_high_gain_db > 0 ? '+' : ''}${cfg.eq_high_gain_db.toFixed(1)} dB`;
    
    // Easy mode Voice Tone slider mapping
    syncEQToToneSlider(cfg);
    
    // Compressor
    dom.compCheckbox.checked = cfg.compressor_enabled;
    dom.compThreshSlider.value = cfg.compressor_threshold_db;
    dom.compThreshVal.textContent = `${cfg.compressor_threshold_db} dB`;
    dom.compRatioSlider.value = cfg.compressor_ratio;
    dom.compRatioVal.textContent = `${cfg.compressor_ratio.toFixed(1)}:1`;
    dom.compAttackSlider.value = cfg.compressor_attack_ms;
    dom.compAttackVal.textContent = `${cfg.compressor_attack_ms} ms`;
    dom.compReleaseSlider.value = cfg.compressor_release_ms;
    dom.compReleaseVal.textContent = `${cfg.compressor_release_ms} ms`;
    
    // De-Esser
    dom.deesserCheckbox.checked = cfg.deesser_enabled;
    dom.deesserThreshSlider.value = cfg.deesser_threshold_db;
    dom.deesserThreshVal.textContent = `${cfg.deesser_threshold_db} dB`;
    dom.deesserAmountSlider.value = Math.round(cfg.deesser_amount * 100);
    dom.deesserAmountVal.textContent = `${Math.round(cfg.deesser_amount * 100)}%`;

    // Vocal Exciter
    dom.exciterCheckbox.checked = cfg.exciter_enabled;
    dom.exciterFreqSlider.value = cfg.exciter_frequency;
    dom.exciterFreqVal.textContent = `${cfg.exciter_frequency} Hz`;
    dom.exciterAmountSlider.value = Math.round(cfg.exciter_amount * 100);
    dom.exciterAmountVal.textContent = `${Math.round(cfg.exciter_amount * 100)}%`;
    dom.exciterMixSlider.value = Math.round(cfg.exciter_mix * 100);
    dom.exciterMixVal.textContent = `${Math.round(cfg.exciter_mix * 100)}%`;
}

function updateNSModeButtons(mode) {
    dom.nsModeOff.classList.remove('active');
    dom.nsModeEco.classList.remove('active');
    dom.nsModeHigh.classList.remove('active');
    
    if (mode === 'off') dom.nsModeOff.classList.add('active');
    if (mode === 'eco') dom.nsModeEco.classList.add('active');
    if (mode === 'high') dom.nsModeHigh.classList.add('active');
}

// --- WebSocket Services ---
function connectWebSocket() {
    socket = new WebSocket(WS_URL);
    
    socket.onopen = () => {
        dom.connIndicator.classList.remove('disconnected');
        dom.connIndicator.classList.add('connected');
        dom.connIndicator.textContent = 'CONNECTED';
    };
    
    socket.onmessage = (event) => {
        const metrics = JSON.parse(event.data);
        updateUIWithMetrics(metrics);
    };
    
    socket.onclose = () => {
        dom.connIndicator.classList.remove('connected');
        dom.connIndicator.classList.add('disconnected');
        dom.connIndicator.textContent = 'DISCONNECTED (RETRYING...)';
        setTimeout(connectWebSocket, 3000);
    };
    
    socket.onerror = (err) => {
        console.error("WebSocket Error:", err);
    };
}

function sendConfigChange(key, value) {
    settings[key] = value;
    if (socket && socket.readyState === WebSocket.OPEN) {
        socket.send(JSON.stringify({
            action: 'update_setting',
            key: key,
            value: value
        }));
    } else {
        fetch(`${API_BASE}/api/settings`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ key, value })
        }).catch(err => console.error("REST update failed:", err));
    }
}

function sendBulkConfig(updates) {
    Object.keys(updates).forEach(key => {
        settings[key] = updates[key];
        if (socket && socket.readyState === WebSocket.OPEN) {
            socket.send(JSON.stringify({
                action: 'update_setting',
                key: key,
                value: updates[key]
            }));
        }
    });
}

function updateUIWithMetrics(m) {
    // Engine State Display
    if (m.is_running) {
        dom.engineStatus.textContent = 'RUNNING';
        dom.engineStatus.className = 'status-value status-running';
        dom.powerBtn.innerHTML = '🛑 STOP ENGINE';
        dom.powerBtn.className = 'power-button power-running';
    } else {
        dom.engineStatus.textContent = 'STOPPED';
        dom.engineStatus.className = 'status-value status-stopped';
        dom.powerBtn.innerHTML = '⚡ START ENGINE';
        dom.powerBtn.className = 'power-button';
    }
    
    // AI Status Display
    if (m.df_available) {
        dom.aiStatus.textContent = 'ACTIVE';
        dom.aiStatus.className = 'status-value status-available';
        dom.dfInfo.innerHTML = "HQ AI Modu Aktif! DeepFilterNet arka planda CPU üzerinde çalışıyor.";
        dom.dfInfo.style.borderColor = "rgba(0, 242, 254, 0.3)";
    } else {
        dom.aiStatus.textContent = 'UNAVAILABLE';
        dom.aiStatus.className = 'status-value status-unavailable';
        dom.dfInfo.innerHTML = "HQ AI kullanılamıyor (PyTorch eksik). Eco DSP modu devrede.";
        dom.dfInfo.style.borderColor = "rgba(255, 0, 127, 0.2)";
    }

    // Gate Status Display
    isGateOpen = m.gate_state;
    if (isGateOpen) {
        dom.gateLed.className = 'led led-open';
        dom.gateText.textContent = 'OPEN (TRANSMITTING)';
        dom.gateText.style.color = 'var(--neon-green)';
    } else {
        dom.gateLed.className = 'led led-closed';
        dom.gateText.textContent = 'MUTED (GATE CLOSED)';
        dom.gateText.style.color = 'var(--neon-pink)';
    }
    
    // Update live metrics values (damped)
    inputLevelSmoothed = inputLevelSmoothed * 0.7 + m.input_rms * 0.3;
    outputLevelSmoothed = outputLevelSmoothed * 0.7 + m.output_rms * 0.3;
    
    rawFFTData = m.fft_data;
}

function dbToPercent(db) {
    if (db <= -60) return 0;
    if (db >= 0) return 100;
    return ((db + 60) / 60) * 100;
}

// --- Presets Settings ---
function applyPreset(presetId) {
    let updates = {};
    
    // Clear active preset buttons
    dom.presetOff.classList.remove('active');
    dom.presetVoice.classList.remove('active');
    dom.presetGaming.classList.remove('active');
    dom.presetBroadcaster.classList.remove('active');
    
    if (presetId === 'off') {
        dom.presetOff.classList.add('active');
        updates = {
            ns_mode: 'off',
            gate_enabled: false,
            eq_enabled: false,
            compressor_enabled: false,
            deesser_enabled: false,
            exciter_enabled: false
        };
    } 
    else if (presetId === 'voice') {
        dom.presetVoice.classList.add('active');
        updates = {
            ns_mode: 'eco',
            ns_eco_strength: 0.8,
            gate_enabled: true,
            gate_threshold_db: -48.0,
            gate_release_ms: 150.0,
            eq_enabled: true,
            eq_low_gain_db: 0.0,
            eq_mid_gain_db: 0.0,
            eq_high_gain_db: 0.0,
            compressor_enabled: true,
            compressor_threshold_db: -18.0,
            compressor_ratio: 3.0,
            compressor_attack_ms: 10.0,
            compressor_release_ms: 100.0,
            deesser_enabled: false,
            exciter_enabled: false
        };
    } 
    else if (presetId === 'gaming') {
        dom.presetGaming.classList.add('active');
        updates = {
            ns_mode: 'eco',
            ns_eco_strength: 0.9,
            gate_enabled: true,
            gate_threshold_db: -38.0, // High threshold for mechanical key clicks
            gate_release_ms: 100.0,
            eq_enabled: true,
            eq_low_gain_db: -2.0,
            eq_mid_gain_db: 2.0,
            eq_high_gain_db: 1.0,
            compressor_enabled: true,
            compressor_threshold_db: -14.0,
            compressor_ratio: 3.5,
            compressor_attack_ms: 5.0,
            compressor_release_ms: 80.0,
            deesser_enabled: false,
            exciter_enabled: false
        };
    } 
    else if (presetId === 'broadcaster') {
        dom.presetBroadcaster.classList.add('active');
        updates = {
            ns_mode: 'eco',
            ns_eco_strength: 0.75,
            gate_enabled: true,
            gate_threshold_db: -50.0,
            gate_release_ms: 200.0,
            eq_enabled: true,
            eq_low_gain_db: 4.5,
            eq_mid_gain_db: 1.0,
            eq_high_gain_db: 3.0,
            compressor_enabled: true,
            compressor_threshold_db: -22.0,
            compressor_ratio: 4.0,
            compressor_attack_ms: 12.0,
            compressor_release_ms: 120.0,
            deesser_enabled: true,
            deesser_threshold_db: -25.0,
            deesser_amount: 0.5,
            exciter_enabled: true,
            exciter_frequency: 3000.0,
            exciter_amount: 0.2,
            exciter_mix: 0.15
        };
    }
    
    sendBulkConfig(updates);
    
    // Update visualizers
    setTimeout(() => {
        applySettingsToUI(Object.assign({}, settings, updates));
    }, 50);
}

function detectActivePreset(cfg) {
    dom.presetOff.classList.remove('active');
    dom.presetVoice.classList.remove('active');
    dom.presetGaming.classList.remove('active');
    dom.presetBroadcaster.classList.remove('active');
    
    if (cfg.ns_mode === 'off' && !cfg.gate_enabled && !cfg.eq_enabled && !cfg.compressor_enabled) {
        dom.presetOff.classList.add('active');
    } 
    else if (cfg.ns_mode === 'eco' && cfg.gate_enabled && cfg.gate_threshold_db === -38.0) {
        dom.presetGaming.classList.add('active');
    } 
    else if (cfg.eq_low_gain_db === 4.5 && cfg.compressor_threshold_db === -22.0) {
        dom.presetBroadcaster.classList.add('active');
    } 
    else if (cfg.ns_mode === 'eco' || cfg.ns_mode === 'high') {
        dom.presetVoice.classList.add('active');
    } 
    else {
        dom.presetOff.classList.add('active');
    }
}

// Map Voice Tone slider values (0, 1, 2)
function applyVoiceToneSlider(val) {
    let label = "Doğal (Natural)";
    let updates = { eq_enabled: true };
    
    if (val === 0) {
        label = "Sıcak (Warm)";
        updates.eq_low_gain_db = 4.0;
        updates.eq_mid_gain_db = 0.5;
        updates.eq_high_gain_db = -3.0;
    } else if (val === 1) {
        label = "Doğal (Natural)";
        updates.eq_low_gain_db = 0.0;
        updates.eq_mid_gain_db = 0.0;
        updates.eq_high_gain_db = 0.0;
    } else if (val === 2) {
        label = "Parlak (Bright)";
        updates.eq_low_gain_db = -2.0;
        updates.eq_mid_gain_db = 1.0;
        updates.eq_high_gain_db = 4.5;
    }
    
    dom.easyToneVal.textContent = label;
    sendBulkConfig(updates);
    
    setTimeout(() => {
        applySettingsToUI(Object.assign({}, settings, updates));
    }, 50);
}

function syncEQToToneSlider(cfg) {
    if (!cfg.eq_enabled) {
        dom.easyToneSlider.value = 1;
        dom.easyToneVal.textContent = "Doğal (Natural)";
        return;
    }
    
    if (cfg.eq_low_gain_db > 2.0 && cfg.eq_high_gain_db < -1.0) {
        dom.easyToneSlider.value = 0;
        dom.easyToneVal.textContent = "Sıcak (Warm)";
    } else if (cfg.eq_low_gain_db < 0.0 && cfg.eq_high_gain_db > 2.0) {
        dom.easyToneSlider.value = 2;
        dom.easyToneVal.textContent = "Parlak (Bright)";
    } else {
        dom.easyToneSlider.value = 1;
        dom.easyToneVal.textContent = "Doğal (Natural)";
    }
}

// --- Setup User Events Listeners ---
function setupEventListeners() {
    // Mode toggles (EASY vs ADVANCED)
    dom.viewModeSimple.onclick = () => {
        dom.viewModeSimple.classList.add('active');
        dom.viewModeAdv.classList.remove('active');
        dom.appWrapper.classList.remove('mode-adv');
        dom.appWrapper.classList.add('mode-simple');
        resizeCanvas();
    };
    
    dom.viewModeAdv.onclick = () => {
        dom.viewModeAdv.classList.add('active');
        dom.viewModeSimple.classList.remove('active');
        dom.appWrapper.classList.remove('mode-simple');
        dom.appWrapper.classList.add('mode-adv');
        resizeCanvas();
    };

    // Studio FX rack tab panel clicks
    const tabButtons = document.querySelectorAll('#fx-tabs .tab-btn');
    tabButtons.forEach(btn => {
        btn.onclick = () => {
            tabButtons.forEach(b => b.classList.remove('active'));
            btn.classList.add('active');
            
            const panels = document.querySelectorAll('.tab-panel');
            panels.forEach(p => p.classList.remove('active'));
            
            const targetId = btn.getAttribute('data-target');
            const targetPanel = document.getElementById(targetId);
            if (targetPanel) {
                targetPanel.classList.add('active');
            }
        };
    });

    // Toggle Audio Pipeline engine power
    dom.powerBtn.onclick = async () => {
        const isRunning = dom.engineStatus.textContent === 'RUNNING';
        const route = isRunning ? '/api/stop' : '/api/start';
        
        try {
            const res = await fetch(`${API_BASE}${route}`, { method: 'POST' });
            if (res.ok) {
                await fetchSettings();
            } else {
                alert("Ses motoru başlatılamadı. Ses aygıtlarını kontrol edin.");
            }
        } catch (e) {
            console.error("Power toggle error", e);
        }
    };
    
    // Refresh audio devices button click handler
    dom.refreshDevicesBtn.onclick = async () => {
        dom.refreshDevicesBtn.classList.add('spinning');
        try {
            await fetchDevices();
        } catch (e) {
            console.error("Failed to refresh devices", e);
        } finally {
            setTimeout(() => {
                dom.refreshDevicesBtn.classList.remove('spinning');
            }, 450);
        }
    };

    // Change Routing selection dropdowns
    dom.inputSelect.onchange = (e) => sendConfigChange('input_device', e.target.value);
    dom.outputSelect.onchange = (e) => {
        sendConfigChange('output_device', e.target.value);
        updateRoutingWarning();
    };
    
    dom.monitorCheckbox.onchange = (e) => {
        const val = e.target.checked;
        dom.monitorSelect.disabled = !val;
        sendConfigChange('monitor_enabled', val);
    };
    dom.monitorSelect.onchange = (e) => sendConfigChange('monitor_device', e.target.value);
    
    // Easy mode preset buttons
    dom.presetOff.onclick = () => applyPreset('off');
    dom.presetVoice.onclick = () => applyPreset('voice');
    dom.presetGaming.onclick = () => applyPreset('gaming');
    dom.presetBroadcaster.onclick = () => applyPreset('broadcaster');
    
    // Easy mode sliders
    dom.easyNSSlider.oninput = (e) => {
        const val = parseInt(e.target.value);
        dom.easyNSVal.textContent = `${val}%`;
        
        dom.nsStrengthSlider.value = val;
        dom.nsStrengthVal.textContent = `${val}%`;
        
        if (settings.ns_mode === 'off') {
            updateNSModeButtons('eco');
            sendConfigChange('ns_mode', 'eco');
        }
        sendConfigChange('ns_eco_strength', val / 100);
    };
    
    dom.easyToneSlider.oninput = (e) => {
        const val = parseInt(e.target.value);
        applyVoiceToneSlider(val);
    };
    
    // Advanced Gains
    dom.inputGainSlider.oninput = (e) => {
        const val = parseFloat(e.target.value);
        dom.inputGainVal.textContent = `${val.toFixed(2)}x`;
        sendConfigChange('input_gain', val);
    };
    dom.outputGainSlider.oninput = (e) => {
        const val = parseFloat(e.target.value);
        dom.outputGainVal.textContent = `${val.toFixed(2)}x`;
        sendConfigChange('output_gain', val);
    };
    
    // Advanced NS mode selection
    dom.nsModeOff.onclick = () => { updateNSModeButtons('off'); sendConfigChange('ns_mode', 'off'); };
    dom.nsModeEco.onclick = () => { updateNSModeButtons('eco'); sendConfigChange('ns_mode', 'eco'); };
    dom.nsModeHigh.onclick = () => { updateNSModeButtons('high'); sendConfigChange('ns_mode', 'high'); };
    
    dom.nsStrengthSlider.oninput = (e) => {
        const val = parseInt(e.target.value);
        dom.nsStrengthVal.textContent = `${val}%`;
        dom.easyNSSlider.value = val;
        dom.easyNSVal.textContent = `${val}%`;
        sendConfigChange('ns_eco_strength', val / 100);
    };
    
    // Advanced Noise Gate
    dom.gateCheckbox.onchange = (e) => {
        const val = e.target.checked;
        sendConfigChange('gate_enabled', val);
        settings.gate_enabled = val;
        updateGateMarkerPosition(settings.gate_threshold_db);
    };
    dom.gateThreshSlider.oninput = (e) => {
        const val = parseInt(e.target.value);
        dom.gateThreshVal.textContent = `${val} dB`;
        sendConfigChange('gate_threshold_db', val);
        settings.gate_threshold_db = val;
        updateGateMarkerPosition(val);
    };
    dom.gateReleaseSlider.oninput = (e) => {
        const val = parseInt(e.target.value);
        dom.gateReleaseVal.textContent = `${val} ms`;
        sendConfigChange('gate_release_ms', val);
    };
    
    // Advanced EQ
    dom.eqCheckbox.onchange = (e) => sendConfigChange('eq_enabled', e.target.checked);
    dom.eqLowSlider.oninput = (e) => {
        const val = parseFloat(e.target.value);
        dom.eqLowVal.textContent = `${val > 0 ? '+' : ''}${val.toFixed(1)} dB`;
        sendConfigChange('eq_low_gain_db', val);
        syncEQToToneSlider(Object.assign({}, settings, { eq_low_gain_db: val }));
    };
    dom.eqMidSlider.oninput = (e) => {
        const val = parseFloat(e.target.value);
        dom.eqMidVal.textContent = `${val > 0 ? '+' : ''}${val.toFixed(1)} dB`;
        sendConfigChange('eq_mid_gain_db', val);
        syncEQToToneSlider(Object.assign({}, settings, { eq_mid_gain_db: val }));
    };
    dom.eqHighSlider.oninput = (e) => {
        const val = parseFloat(e.target.value);
        dom.eqHighVal.textContent = `${val > 0 ? '+' : ''}${val.toFixed(1)} dB`;
        sendConfigChange('eq_high_gain_db', val);
        syncEQToToneSlider(Object.assign({}, settings, { eq_high_gain_db: val }));
    };
    
    // Advanced Compressor
    dom.compCheckbox.onchange = (e) => sendConfigChange('compressor_enabled', e.target.checked);
    dom.compThreshSlider.oninput = (e) => {
        const val = parseInt(e.target.value);
        dom.compThreshVal.textContent = `${val} dB`;
        sendConfigChange('compressor_threshold_db', val);
    };
    dom.compRatioSlider.oninput = (e) => {
        const val = parseFloat(e.target.value);
        dom.compRatioVal.textContent = `${val.toFixed(1)}:1`;
        sendConfigChange('compressor_ratio', val);
    };
    dom.compAttackSlider.oninput = (e) => {
        const val = parseInt(e.target.value);
        dom.compAttackVal.textContent = `${val} ms`;
        sendConfigChange('compressor_attack_ms', val);
    };
    dom.compReleaseSlider.oninput = (e) => {
        const val = parseInt(e.target.value);
        dom.compReleaseVal.textContent = `${val} ms`;
        sendConfigChange('compressor_release_ms', val);
    };
    
    // Advanced De-Esser
    dom.deesserCheckbox.onchange = (e) => sendConfigChange('deesser_enabled', e.target.checked);
    dom.deesserThreshSlider.oninput = (e) => {
        const val = parseInt(e.target.value);
        dom.deesserThreshVal.textContent = `${val} dB`;
        sendConfigChange('deesser_threshold_db', val);
    };
    dom.deesserAmountSlider.oninput = (e) => {
        const val = parseInt(e.target.value);
        dom.deesserAmountVal.textContent = `${val}%`;
        sendConfigChange('deesser_amount', val / 100);
    };

    // Advanced Vocal Exciter
    dom.exciterCheckbox.onchange = (e) => sendConfigChange('exciter_enabled', e.target.checked);
    dom.exciterFreqSlider.oninput = (e) => {
        const val = parseInt(e.target.value);
        dom.exciterFreqVal.textContent = `${val} Hz`;
        sendConfigChange('exciter_frequency', val);
    };
    dom.exciterAmountSlider.oninput = (e) => {
        const val = parseInt(e.target.value);
        dom.exciterAmountVal.textContent = `${val}%`;
        sendConfigChange('exciter_amount', val / 100);
    };
    dom.exciterMixSlider.oninput = (e) => {
        const val = parseInt(e.target.value);
        dom.exciterMixVal.textContent = `${val}%`;
        sendConfigChange('exciter_mix', val / 100);
    };
    
    // Visualizer Mode
    dom.vizTabSpec.onclick = () => {
        dom.vizTabSpec.classList.add('active');
        dom.vizTabWave.classList.remove('active');
        currentVizMode = 'spec';
    };
    dom.vizTabWave.onclick = () => {
        dom.vizTabWave.classList.add('active');
        dom.vizTabSpec.classList.remove('active');
        currentVizMode = 'wave';
    };
}

// --- High-Performance Animation/Drawing Loop ---
function startVisualizationLoop() {
    function draw() {
        requestAnimationFrame(draw);
        
        // Render volume level meters using GPU scaleX (Fast, no reflow)
        const inPercent = dbToPercent(inputLevelSmoothed);
        const outPercent = dbToPercent(outputLevelSmoothed);
        
        dom.inputMeterFill.style.transform = `scaleX(${inPercent / 100})`;
        dom.outputMeterFill.style.transform = `scaleX(${outPercent / 100})`;
        
        // Clear canvas
        const w = dom.canvas.width / (window.devicePixelRatio || 1);
        const h = dom.canvas.height / (window.devicePixelRatio || 1);
        ctx.fillStyle = '#08080c';
        ctx.fillRect(0, 0, w, h);
        
        // Smooth FFT bins
        for (let i = 0; i < 64; i++) {
            const rawVal = rawFFTData[i] || 0;
            smoothedFFT[i] = smoothedFFT[i] * 0.8 + rawVal * 0.2;
        }
        
        if (currentVizMode === 'spec') {
            drawSpectrum(w, h);
        } else {
            drawWaveform(w, h);
        }
    }
    requestAnimationFrame(draw);
}

function drawSpectrum(w, h) {
    const barCount = 36;
    const padding = 2.5;
    const barWidth = (w - (barCount - 1) * padding) / barCount;
    
    for (let i = 0; i < barCount; i++) {
        const binIdx = Math.floor((i / barCount) * 40);
        const val = smoothedFFT[binIdx] || 0;
        
        const barHeight = Math.max(2, val * (h - 15));
        const x = i * (barWidth + padding);
        const y = h - barHeight;
        
        const grad = ctx.createLinearGradient(0, h, 0, y);
        grad.addColorStop(0, '#00f2fe');
        grad.addColorStop(0.6, '#9d4edd');
        grad.addColorStop(1, '#ff007f');
        
        ctx.fillStyle = grad;
        
        ctx.beginPath();
        ctx.roundRect(x, y, barWidth, barHeight, [2, 2, 0, 0]);
        ctx.fill();
        
        // High frequency glow lines (no shadowBlur filter for maximum performance)
        if (val > 0.45) {
            ctx.fillStyle = '#ffffff';
            ctx.fillRect(x, y, barWidth, 1.5);
        }
    }
}

function drawWaveform(w, h) {
    const centerY = h / 2;
    const points = 80;
    
    let energySum = smoothedFFT.reduce((a, b) => a + b, 0) / 64;
    if (!isGateOpen) energySum = 0;
    
    ctx.beginPath();
    for (let i = 0; i < points; i++) {
        const x = (i / (points - 1)) * w;
        
        let displacement = 0;
        if (energySum > 0.04) {
            const time = Date.now() * 0.015;
            displacement = 
                Math.sin(i * 0.2 + time) * 10 * (smoothedFFT[4] || 0.1) +
                Math.sin(i * 0.4 - time * 1.2) * 6 * (smoothedFFT[10] || 0.1);
            
            const taper = Math.sin((i / (points - 1)) * Math.PI);
            displacement *= taper * (energySum * 2.0);
        }
        
        const y = centerY + displacement;
        if (i === 0) {
            ctx.moveTo(x, y);
        } else {
            ctx.lineTo(x, y);
        }
    }
    
    // Draw a high performance dual-stroke glow effect
    ctx.lineWidth = 4;
    ctx.strokeStyle = 'rgba(0, 242, 254, 0.2)';
    ctx.stroke();
    
    ctx.lineWidth = 1.5;
    ctx.strokeStyle = '#00f2fe';
    ctx.stroke();
}

// Start application
window.onload = init;
