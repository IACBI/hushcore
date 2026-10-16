/* HushCore dashboard client.
   REST covers settings, devices, presets and engine control. The WebSocket streams
   live metrics and carries fast setting updates. */

const API_KEY = window.HUSHCORE_API_KEY || '';
const API_BASE = window.location.origin;
// The key is sent as the first WebSocket message, not in the URL, so it stays out of
// server logs and browser history. See connectWebSocket().
const WS_URL = `${window.location.protocol === 'https:' ? 'wss:' : 'ws:'}//${window.location.host}/ws`;
const WS_AUTH_FAILED_CODE = 4001;
const WS_AUTH_FAILED_LABEL = 'AUTHENTICATION FAILED - RELOAD THE PAGE';

const WS_RETRY_MIN_MS = 1000;
const WS_RETRY_MAX_MS = 10000;
const SETTING_SEND_INTERVAL_MS = 50; // caps slider traffic at about 20 messages/s per control
const CLIP_HOLD_MS = 900;
// Float settings pass through JSON and backend coercion, so exact equality would miss
// presets that were applied correctly.
const FLOAT_TOLERANCE = 1e-4;
const TOAST_LIMIT = 3;
const DELETE_ARM_MS = 3000;

const BUILTIN_LABELS = { off: 'Bypass', voice: 'Clean voice', gaming: 'Gaming', broadcaster: 'Broadcaster' };
// Easy-mode tone slider steps. These shape the EQ only and are not presets.
const TONE_STEPS = [
    { label: 'Warm', low: 4.0, mid: 0.5, high: -3.0 },
    { label: 'Natural', low: 0.0, mid: 0.0, high: 0.0 },
    { label: 'Bright', low: -2.0, mid: 1.0, high: 4.5 },
];
// Windows reports device names in the OS language, so the Turkish keywords are needed.
const VIRTUAL_OUTPUT_HINTS = ['cable', 'virtual', 'vb-audio', 'voicemeeter'];

class ApiError extends Error {
    constructor(message, status) {
        super(message);
        this.name = 'ApiError';
        this.status = status;
    }
}

// --- Shared state ---------------------------------------------------------
let socket = null;
// True only between the backend's auth_ok and the socket closing. Settings go over the
// socket only while this is set; otherwise they fall back to REST.
let socketReady = false;
let reconnectTimer = null;
let reconnectDelay = WS_RETRY_MIN_MS;

let settings = {};
let presets = { builtin: {}, user: {} };
let engineRunning = null;
let aiAvailable = null;
let gateOpen = null;
let powerBusy = false;
let isVstFailed = false;
let armedDelete = null;
let clipTimer = 0;
let lastSaveErrorAt = 0;
const throttles = new Map();

// Visualizer state
let currentVizMode = 'spec';
const rawFFTData = new Array(64).fill(0);
const smoothedFFT = new Array(64).fill(0);
const cascadeHistory = [];
const maxCascadeFrames = 30;
let inputLevelSmoothed = -60;
let outputLevelSmoothed = -60;
let canvasW = 0;
let canvasH = 0;
let dpr = 1;
let barGradient = null;
let rafId = 0;
const reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)');

// --- DOM cache ------------------------------------------------------------
const $ = (id) => document.getElementById(id);
const dom = {
    appWrapper: $('app-wrapper'),
    viewModeSimple: $('view-mode-simple'),
    viewModeAdv: $('view-mode-adv'),
    powerBtn: $('power-btn'),
    engineStatus: $('engine-status'),
    aiStatus: $('ai-status'),
    connIndicator: $('connection-indicator'),
    toastRegion: $('toast-region'),
    refreshDevicesBtn: $('refresh-devices-btn'),

    inputSelect: $('input-device-select'),
    outputSelect: $('output-device-select'),
    routingTipBox: $('routing-tip-box'),
    monitorCheckbox: $('monitor-enabled-checkbox'),
    monitorSelect: $('monitor-device-select'),
    bufferSizeSelect: $('buffer-size-select'),

    inputGainSlider: $('input-gain-slider'),
    inputGainVal: $('input-gain-val'),
    outputGainSlider: $('output-gain-slider'),
    outputGainVal: $('output-gain-val'),
    statusRate: $('status-rate'),
    statusLatency: $('status-latency'),
    statusCpu: $('status-cpu'),
    inputMeterFill: $('input-meter-fill'),
    outputMeterFill: $('output-meter-fill'),
    gateMarker: $('gate-threshold-marker'),
    clipIndicator: $('clip-indicator'),

    canvas: $('visualizer-canvas'),
    vizTabSpec: $('viz-tab-spec'),
    vizTabPulse: $('viz-tab-pulse'),
    vizTabCascade: $('viz-tab-cascade'),
    gateLed: $('gate-led'),
    gateText: $('gate-text'),

    userPresetList: $('user-preset-list'),
    savePresetForm: $('save-preset-form'),
    savePresetInput: $('save-preset-input'),
    savePresetBtn: $('save-preset-btn'),
    easyNSSlider: $('easy-ns-slider'),
    easyNSVal: $('easy-ns-val'),
    easyToneSlider: $('easy-tone-slider'),
    easyToneVal: $('easy-tone-val'),

    nsModeOff: $('ns-mode-off'),
    nsModeEco: $('ns-mode-eco'),
    nsModeHigh: $('ns-mode-high'),
    dfInfo: $('df-availability-info'),
    nsStrengthSlider: $('ns-strength-slider'),
    nsStrengthVal: $('ns-strength-val'),
    fxTabs: $('fx-tabs'),

    gateCheckbox: $('gate-enabled-checkbox'),
    gateThreshSlider: $('gate-thresh-slider'),
    gateThreshVal: $('gate-thresh-val'),
    gateReleaseSlider: $('gate-release-slider'),
    gateReleaseVal: $('gate-release-val'),

    eqCheckbox: $('eq-enabled-checkbox'),
    eqLowSlider: $('eq-low-slider'),
    eqLowVal: $('eq-low-val'),
    eqMidSlider: $('eq-mid-slider'),
    eqMidVal: $('eq-mid-val'),
    eqHighSlider: $('eq-high-slider'),
    eqHighVal: $('eq-high-val'),

    compCheckbox: $('compressor-enabled-checkbox'),
    compThreshSlider: $('comp-thresh-slider'),
    compThreshVal: $('comp-thresh-val'),
    compRatioSlider: $('comp-ratio-slider'),
    compRatioVal: $('comp-ratio-val'),
    compAttackSlider: $('comp-attack-slider'),
    compAttackVal: $('comp-attack-val'),
    compReleaseSlider: $('comp-release-slider'),
    compReleaseVal: $('comp-release-val'),

    deesserCheckbox: $('deesser-enabled-checkbox'),
    deesserThreshSlider: $('deesser-thresh-slider'),
    deesserThreshVal: $('deesser-thresh-val'),
    deesserAmountSlider: $('deesser-amount-slider'),
    deesserAmountVal: $('deesser-amount-val'),

    exciterCheckbox: $('exciter-enabled-checkbox'),
    exciterFreqSlider: $('exciter-freq-slider'),
    exciterFreqVal: $('exciter-freq-val'),
    exciterAmountSlider: $('exciter-amount-slider'),
    exciterAmountVal: $('exciter-amount-val'),
    exciterMixSlider: $('exciter-mix-slider'),
    exciterMixVal: $('exciter-mix-val'),

    vstCheckbox: $('vst-enabled-checkbox'),
    vstPathInput: $('vst-path-input'),
    vstLoadBtn: $('vst-load-btn'),
    vstEditorBtn: $('vst-editor-btn'),
};

// --- Generic helpers ------------------------------------------------------
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function setText(el, text) {
    if (el.textContent !== text) el.textContent = text;
}

function setPressed(el, on) {
    el.classList.toggle('active', on);
    el.setAttribute('aria-pressed', String(on));
}

function finiteOrNull(v) {
    return typeof v === 'number' && Number.isFinite(v) ? v : null;
}

// Single entry point for REST. Throws ApiError carrying the backend's detail message.
async function api(path, { method = 'GET', body } = {}) {
    const headers = { 'X-API-Key': API_KEY };
    const init = { method, headers };
    if (body !== undefined) {
        headers['Content-Type'] = 'application/json';
        init.body = JSON.stringify(body);
    }
    const res = await fetch(`${API_BASE}${path}`, init);
    const text = await res.text();
    let data = null;
    if (text) {
        try {
            data = JSON.parse(text);
        } catch {
            data = text;
        }
    }
    if (!res.ok) {
        const detail = data && typeof data === 'object' ? data.detail : data;
        const reason = typeof detail === 'string' && detail ? detail : res.statusText;
        throw new ApiError(reason ? `${res.status}: ${reason}` : `HTTP ${res.status}`, res.status);
    }
    return data;
}

// --- Toasts ---------------------------------------------------------------
function showToast(message, variant = 'info', durationMs = 3500) {
    const region = dom.toastRegion;
    while (region.children.length >= TOAST_LIMIT) region.firstElementChild.remove();

    const el = document.createElement('div');
    el.className = `toast toast-${variant}`;
    el.textContent = message;
    region.appendChild(el);
    requestAnimationFrame(() => el.classList.add('show'));

    setTimeout(() => {
        el.classList.remove('show');
        setTimeout(() => el.remove(), 300);
    }, durationMs);
}

// --- Formatting -----------------------------------------------------------
const fmtGain = (v) => `${v.toFixed(2)}x`;
const fmtPct = (v) => `${v}%`;
const fmtDb = (v) => `${v} dB`;
const fmtSignedDb = (v) => `${v > 0 ? '+' : ''}${v.toFixed(1)} dB`;
const fmtMs = (v) => `${v} ms`;
const fmtRatio = (v) => `${v.toFixed(1)}:1`;
const fmtHz = (v) => `${v} Hz`;

// --- Range controls -------------------------------------------------------
// Each slider maps to one setting key. Several sliders may share a key (the Easy and
// Advanced noise-reduction sliders); they are kept in sync.
const identity = (v) => v;
const PERCENT = {
    toUI: (v) => Math.round(v * 100),
    toStored: (v) => v / 100,
};

function range(key, slider, out, fmt, opts = {}) {
    return { key, slider, out, fmt, toUI: identity, toStored: identity, ...opts };
}

const RANGE_CONTROLS = [
    range('input_gain', dom.inputGainSlider, dom.inputGainVal, fmtGain),
    range('output_gain', dom.outputGainSlider, dom.outputGainVal, fmtGain),
    range('ns_eco_strength', dom.easyNSSlider, dom.easyNSVal, fmtPct, {
        ...PERCENT,
        onInput: () => { if (settings.ns_mode === 'off') setNSMode('eco'); },
    }),
    range('ns_eco_strength', dom.nsStrengthSlider, dom.nsStrengthVal, fmtPct, PERCENT),
    range('gate_threshold_db', dom.gateThreshSlider, dom.gateThreshVal, fmtDb, {
        onInput: () => updateGateMarker(settings),
    }),
    range('gate_release_ms', dom.gateReleaseSlider, dom.gateReleaseVal, fmtMs),
    range('eq_low_gain_db', dom.eqLowSlider, dom.eqLowVal, fmtSignedDb, { onInput: () => syncToneSlider(settings) }),
    range('eq_mid_gain_db', dom.eqMidSlider, dom.eqMidVal, fmtSignedDb, { onInput: () => syncToneSlider(settings) }),
    range('eq_high_gain_db', dom.eqHighSlider, dom.eqHighVal, fmtSignedDb, { onInput: () => syncToneSlider(settings) }),
    range('compressor_threshold_db', dom.compThreshSlider, dom.compThreshVal, fmtDb),
    range('compressor_ratio', dom.compRatioSlider, dom.compRatioVal, fmtRatio),
    range('compressor_attack_ms', dom.compAttackSlider, dom.compAttackVal, fmtMs),
    range('compressor_release_ms', dom.compReleaseSlider, dom.compReleaseVal, fmtMs),
    range('deesser_threshold_db', dom.deesserThreshSlider, dom.deesserThreshVal, fmtDb),
    range('deesser_amount', dom.deesserAmountSlider, dom.deesserAmountVal, fmtPct, PERCENT),
    range('exciter_frequency', dom.exciterFreqSlider, dom.exciterFreqVal, fmtHz),
    range('exciter_amount', dom.exciterAmountSlider, dom.exciterAmountVal, fmtPct, PERCENT),
    range('exciter_mix', dom.exciterMixSlider, dom.exciterMixVal, fmtPct, PERCENT),
];

function renderRange(c, stored) {
    const ui = c.toUI(stored);
    c.slider.value = ui;
    c.out.textContent = c.fmt(ui);
}

function bindRange(c) {
    c.slider.addEventListener('input', () => {
        const ui = Number(c.slider.value);
        const stored = c.toStored(ui);
        c.out.textContent = c.fmt(ui);
        for (const other of RANGE_CONTROLS) {
            if (other !== c && other.key === c.key) renderRange(other, stored);
        }
        sendThrottled(c.key, stored);
        if (c.onInput) c.onInput(stored);
    });
}

// Checkbox toggles
const CHECK_CONTROLS = [
    { key: 'monitor_enabled', el: dom.monitorCheckbox, onChange: () => { dom.monitorSelect.disabled = !settings.monitor_enabled; } },
    { key: 'gate_enabled', el: dom.gateCheckbox, onChange: () => updateGateMarker(settings) },
    { key: 'eq_enabled', el: dom.eqCheckbox, onChange: () => syncToneSlider(settings) },
    { key: 'compressor_enabled', el: dom.compCheckbox },
    { key: 'deesser_enabled', el: dom.deesserCheckbox },
    { key: 'exciter_enabled', el: dom.exciterCheckbox },
    { key: 'vst_enabled', el: dom.vstCheckbox },
];

const NS_BUTTONS = { off: dom.nsModeOff, eco: dom.nsModeEco, high: dom.nsModeHigh };
const VIZ_BUTTONS = { spec: dom.vizTabSpec, pulse: dom.vizTabPulse, cascade: dom.vizTabCascade };

// --- Canvas ---------------------------------------------------------------
const ctx = dom.canvas.getContext('2d');

function resizeCanvas() {
    dpr = window.devicePixelRatio || 1;
    const rect = dom.canvas.getBoundingClientRect();
    canvasW = rect.width;
    canvasH = rect.height;
    dom.canvas.width = Math.max(1, Math.round(canvasW * dpr));
    dom.canvas.height = Math.max(1, Math.round(canvasH * dpr));
    // Assigning width/height resets the context, and setTransform replaces the
    // transform outright. Calling scale() here would compound on every resize.
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

    barGradient = ctx.createLinearGradient(0, canvasH, 0, 0);
    barGradient.addColorStop(0, '#00f2fe');
    barGradient.addColorStop(0.6, '#9d4edd');
    barGradient.addColorStop(1, '#ff007f');
}

resizeCanvas();
new ResizeObserver(() => resizeCanvas()).observe(dom.canvas.parentElement);
window.addEventListener('resize', resizeCanvas);

function startVisualizer() {
    if (!rafId && !document.hidden) rafId = requestAnimationFrame(drawFrame);
}

function stopVisualizer() {
    cancelAnimationFrame(rafId);
    rafId = 0;
}

document.addEventListener('visibilitychange', () => {
    if (document.hidden) stopVisualizer();
    else startVisualizer();
});

function drawFrame() {
    rafId = requestAnimationFrame(drawFrame);

    dom.inputMeterFill.style.transform = `scaleX(${dbToPercent(inputLevelSmoothed) / 100})`;
    dom.outputMeterFill.style.transform = `scaleX(${dbToPercent(outputLevelSmoothed) / 100})`;

    for (let i = 0; i < 64; i++) {
        smoothedFFT[i] = smoothedFFT[i] * 0.8 + (rawFFTData[i] || 0) * 0.2;
    }

    ctx.fillStyle = '#08080c';
    ctx.fillRect(0, 0, canvasW, canvasH);

    if (currentVizMode === 'spec') {
        drawSpectrum(canvasW, canvasH);
    } else if (currentVizMode === 'pulse') {
        drawPulse(canvasW, canvasH);
    } else {
        cascadeHistory.push(smoothedFFT.slice());
        if (cascadeHistory.length > maxCascadeFrames) cascadeHistory.shift();
        drawCascade(canvasW, canvasH);
    }
}

function drawSpectrum(w, h) {
    const barCount = 36;
    const padding = 2.5;
    const barWidth = (w - (barCount - 1) * padding) / barCount;

    for (let i = 0; i < barCount; i++) {
        const val = smoothedFFT[Math.floor((i / barCount) * 40)] || 0;
        const barHeight = Math.max(2, val * (h - 15));
        const x = i * (barWidth + padding);
        const y = h - barHeight;

        ctx.fillStyle = barGradient;
        ctx.beginPath();
        if (ctx.roundRect) ctx.roundRect(x, y, barWidth, barHeight, [2, 2, 0, 0]);
        else ctx.rect(x, y, barWidth, barHeight);
        ctx.fill();

        // Peak cap; a solid fill is cheaper than a shadow blur on every bar.
        if (val > 0.45) {
            ctx.fillStyle = '#ffffff';
            ctx.fillRect(x, y, barWidth, 1.5);
        }
    }
}

// Not a real waveform: the backend sends FFT bins only, so this is driven by band energy.
function drawPulse(w, h) {
    const centerY = h / 2;
    const points = 80;

    let energy = smoothedFFT.reduce((a, b) => a + b, 0) / 64;
    if (!gateOpen) energy = 0;
    const time = reduceMotion.matches ? 0 : Date.now() * 0.015;

    ctx.beginPath();
    for (let i = 0; i < points; i++) {
        const x = (i / (points - 1)) * w;
        let displacement = 0;
        if (energy > 0.04) {
            displacement =
                Math.sin(i * 0.2 + time) * 10 * (smoothedFFT[4] || 0.1) +
                Math.sin(i * 0.4 - time * 1.2) * 6 * (smoothedFFT[10] || 0.1);
            const taper = Math.sin((i / (points - 1)) * Math.PI);
            displacement *= taper * (energy * 2.0);
        }
        const y = centerY + displacement;
        if (i === 0) ctx.moveTo(x, y);
        else ctx.lineTo(x, y);
    }

    ctx.lineWidth = 4;
    ctx.strokeStyle = 'rgba(0, 242, 254, 0.2)';
    ctx.stroke();

    ctx.lineWidth = 1.5;
    ctx.strokeStyle = '#00f2fe';
    ctx.stroke();
}

function drawCascade(w, h) {
    if (cascadeHistory.length === 0) return;

    const points = 36;
    const stepX = w / (points - 1);
    const offsetX = 0.8;
    const offsetY = 2.2;

    for (let f = 0; f < cascadeHistory.length; f++) {
        const fftFrame = cascadeHistory[f];
        const pct = f / (cascadeHistory.length - 1); // 0 = oldest, 1 = newest

        const startXOffset = (cascadeHistory.length - 1 - f) * offsetX;
        const startYOffset = (cascadeHistory.length - 1 - f) * offsetY;

        ctx.beginPath();
        for (let i = 0; i < points; i++) {
            const x = startXOffset + i * stepX * 0.95;
            const amplitude = (fftFrame[i] || 0) * (h * 0.35);
            const y = (h - 10) - startYOffset - amplitude;
            if (i === 0) ctx.moveTo(x, y);
            else ctx.lineTo(x, y);
        }

        ctx.lineWidth = 1 + pct * 0.8;
        const alpha = 0.08 + pct * 0.5;
        const hue = 180 + (1 - pct) * 120; // neon cyan to neon purple
        ctx.strokeStyle = `hsla(${hue}, 100%, 65%, ${alpha})`;
        ctx.stroke();
    }
}

function setVizMode(mode) {
    currentVizMode = mode;
    for (const [name, btn] of Object.entries(VIZ_BUTTONS)) setPressed(btn, name === mode);
}

function dbToPercent(db) {
    if (db <= -60) return 0;
    if (db >= 0) return 100;
    return ((db + 60) / 60) * 100;
}

// --- Data loading ---------------------------------------------------------
async function fetchSettings() {
    settings = await api('/api/settings');
    applySettingsToUI(settings);
}

async function fetchDevices() {
    populateDevices(await api('/api/devices'));
}

async function fetchPresets() {
    const data = await api('/api/presets');
    presets = { builtin: data.builtin || {}, user: data.user || {} };
    renderUserPresets();
}

async function loadAll() {
    const failures = [];
    const steps = [['settings', fetchSettings], ['devices', fetchDevices], ['presets', fetchPresets]];
    for (const [name, load] of steps) {
        try {
            await load();
        } catch (err) {
            console.error(`Failed to load ${name}:`, err);
            failures.push(name);
        }
    }
    if (failures.length) {
        showToast(`Could not load ${failures.join(', ')}. Check that HushCore is running.`, 'error', 6000);
    }
}

// --- Devices & routing ----------------------------------------------------
function fillSelect(select, items, placeholder) {
    const nodes = [];
    if (placeholder !== undefined) nodes.push(new Option(placeholder, ''));
    for (const d of items) nodes.push(new Option(d.name, d.name));
    select.replaceChildren(...nodes);
}

function populateDevices(list) {
    fillSelect(dom.inputSelect, list.inputs, 'System default microphone');
    if (settings.input_device) dom.inputSelect.value = settings.input_device;

    fillSelect(dom.outputSelect, list.outputs, 'Auto (virtual cable if installed)');
    if (settings.output_device) dom.outputSelect.value = settings.output_device;

    fillSelect(dom.monitorSelect, list.outputs, 'Select headphones...');
    if (settings.monitor_device) dom.monitorSelect.value = settings.monitor_device;

    updateRoutingWarning();
}

function isVirtualOutput(name) {
    const lower = name.toLowerCase();
    return VIRTUAL_OUTPUT_HINTS.some((hint) => lower.includes(hint));
}

function setRoutingBanner(variant, title, body) {
    const box = dom.routingTipBox;
    box.className = `warning-banner ${variant}`;
    box.replaceChildren();
    if (title) {
        const strong = document.createElement('strong');
        strong.textContent = title;
        box.append(strong, document.createElement('br'));
    }
    box.append(body);
}

function updateRoutingWarning() {
    const selected = dom.outputSelect.value;
    if (!selected) {
        setRoutingBanner('warning', 'Automatic output.', 'HushCore uses a virtual cable if one is installed, otherwise your default speakers. Pick CABLE Input to be sure.');
        return;
    }
    if (isVirtualOutput(selected)) {
        setRoutingBanner(
            'success',
            'Virtual cable active.',
            'Audio is sent to the virtual cable. In Discord or OBS, choose "VB-Cable Output" as the microphone.'
        );
    } else {
        setRoutingBanner(
            'warning',
            'Not a virtual cable.',
            'Other apps will not receive this audio, and a speaker output lets you hear yourself. To use HushCore in Discord, Zoom or OBS, route the output to a virtual cable such as VB-Cable.'
        );
    }
}

// --- Settings writes ------------------------------------------------------
function reportSaveFailure(key, err) {
    console.error(`Failed to save ${key}:`, err);
    // Slider drags can fail many times in a row; one toast per burst is enough.
    const now = Date.now();
    if (now - lastSaveErrorAt > 5000) {
        lastSaveErrorAt = now;
        showToast('Could not save a setting. Check the connection to HushCore.', 'error');
    }
}

// Prefers the open socket. Falls back to REST so changes made while the socket is down are not lost.
function sendSettingUpdate(key, value) {
    if (socket && socketReady && socket.readyState === WebSocket.OPEN) {
        socket.send(JSON.stringify({ action: 'update_setting', key, value }));
        return Promise.resolve();
    }
    return api('/api/settings', { method: 'POST', body: { key, value } });
}

function pushSetting(key, value) {
    sendSettingUpdate(key, value).catch((err) => reportSaveFailure(key, err));
}

function sendConfigChange(key, value) {
    settings[key] = value;
    updateActivePreset();
    pushSetting(key, value);
}

// Slider updates: sends the first change at once, then at most one message per interval,
// always carrying the latest value.
function sendThrottled(key, value) {
    settings[key] = value;
    updateActivePreset();

    let slot = throttles.get(key);
    if (!slot) {
        slot = { timer: null, pending: false, latest: value };
        throttles.set(key, slot);
    }
    slot.latest = value;

    if (slot.timer) {
        slot.pending = true;
        return;
    }

    pushSetting(key, value);
    const flush = () => {
        if (slot.pending) {
            slot.pending = false;
            pushSetting(key, slot.latest);
            slot.timer = setTimeout(flush, SETTING_SEND_INTERVAL_MS);
        } else {
            slot.timer = null;
        }
    };
    slot.timer = setTimeout(flush, SETTING_SEND_INTERVAL_MS);
}

function sendBulkConfig(updates) {
    Object.assign(settings, updates);
    updateActivePreset();
    for (const [key, value] of Object.entries(updates)) pushSetting(key, value);
}

// --- WebSocket ------------------------------------------------------------
function setConnectionState(connected, label = connected ? 'CONNECTED' : 'DISCONNECTED - RETRYING') {
    dom.connIndicator.classList.toggle('connected', connected);
    dom.connIndicator.classList.toggle('disconnected', !connected);
    setText(dom.connIndicator, label);
}

function connectWebSocket() {
    if (socket && (socket.readyState === WebSocket.OPEN || socket.readyState === WebSocket.CONNECTING)) return;
    clearTimeout(reconnectTimer);
    reconnectTimer = null;

    const ws = new WebSocket(WS_URL);
    socket = ws;
    socketReady = false;

    ws.onopen = () => {
        ws.send(JSON.stringify({ action: 'auth', token: API_KEY }));
    };

    let authenticated = false;
    ws.onmessage = (event) => {
        let msg;
        try {
            msg = JSON.parse(event.data);
        } catch {
            return;
        }
        if (!authenticated) {
            if (msg && msg.action === 'auth_ok' && socket === ws) {
                authenticated = true;
                socketReady = true;
                reconnectDelay = WS_RETRY_MIN_MS;
                setConnectionState(true);
            }
            return;
        }
        updateUIWithMetrics(msg);
    };

    ws.onclose = (event) => {
        if (socket !== ws) return;
        socket = null;
        socketReady = false;
        if (event.code === WS_AUTH_FAILED_CODE) {
            // The key will not change until the page reloads, so retrying would only spam the server.
            setConnectionState(false, WS_AUTH_FAILED_LABEL);
            return;
        }
        setConnectionState(false);
        scheduleReconnect();
    };

    ws.onerror = () => {
        console.warn('HushCore WebSocket error. Reconnecting.');
    };
}

function scheduleReconnect() {
    if (reconnectTimer) return;
    reconnectTimer = setTimeout(() => {
        reconnectTimer = null;
        connectWebSocket();
    }, reconnectDelay);
    reconnectDelay = Math.min(reconnectDelay * 2, WS_RETRY_MAX_MS);
}

// --- Live metrics ---------------------------------------------------------
function updateUIWithMetrics(m) {
    setEngineState(Boolean(m.is_running));
    setAiState(Boolean(m.df_available));
    setGateState(Boolean(m.gate_state));

    inputLevelSmoothed = inputLevelSmoothed * 0.7 + (m.input_rms ?? -60) * 0.3;
    outputLevelSmoothed = outputLevelSmoothed * 0.7 + (m.output_rms ?? -60) * 0.3;

    if (Array.isArray(m.fft_data)) rawFFTData.splice(0, rawFFTData.length, ...m.fft_data);

    isVstFailed = Boolean(m.vst_failed);

    updateStatusStrip(m);
    if (m.clipping === true) flashClip();
}

function setEngineState(running) {
    if (running === engineRunning) return;
    engineRunning = running;
    dom.engineStatus.textContent = running ? 'RUNNING' : 'STOPPED';
    dom.engineStatus.className = `status-value ${running ? 'status-running' : 'status-stopped'}`;
    dom.powerBtn.textContent = running ? 'STOP ENGINE' : 'START ENGINE';
    dom.powerBtn.classList.toggle('power-running', running);
}

function setAiState(available) {
    if (available === aiAvailable) return;
    aiAvailable = available;
    dom.aiStatus.textContent = available ? 'ACTIVE' : 'UNAVAILABLE';
    dom.aiStatus.className = `status-value ${available ? 'status-available' : 'status-unavailable'}`;
    dom.dfInfo.textContent = available
        ? 'HQ AI is active. DeepFilterNet runs on the CPU.'
        : 'HQ AI is unavailable (PyTorch not installed). Eco DSP is in use.';
    dom.dfInfo.classList.toggle('ok', available);
    dom.dfInfo.classList.toggle('off', !available);
}

function setGateState(open) {
    if (open === gateOpen) return;
    gateOpen = open;
    dom.gateLed.className = `led ${open ? 'led-open' : 'led-closed'}`;
    dom.gateText.textContent = open ? 'OPEN (TRANSMITTING)' : 'MUTED (GATE CLOSED)';
    dom.gateText.className = open ? 'gate-open' : 'gate-closed';
}

function updateStatusStrip(m) {
    const rate = finiteOrNull(m.sample_rate);
    const latency = finiteOrNull(m.latency_ms);
    const cpu = finiteOrNull(m.cpu_load);

    setText(dom.statusRate, rate && rate > 0 ? `${(rate / 1000).toFixed(1).replace(/\.0$/, '')} kHz` : '-- kHz');
    setText(dom.statusLatency, latency !== null ? `${Math.round(latency)} ms` : '-- ms');
    setText(dom.statusCpu, cpu !== null ? `CPU ${Math.round(Math.min(100, Math.max(0, cpu)))}%` : 'CPU --');
}

function flashClip() {
    dom.clipIndicator.classList.add('lit');
    clearTimeout(clipTimer);
    clipTimer = setTimeout(() => dom.clipIndicator.classList.remove('lit'), CLIP_HOLD_MS);
}

// --- Applying settings to the UI -----------------------------------------
function applySettingsToUI(cfg = settings) {
    for (const c of CHECK_CONTROLS) {
        if (cfg[c.key] !== undefined) c.el.checked = Boolean(cfg[c.key]);
    }
    for (const c of RANGE_CONTROLS) {
        if (cfg[c.key] !== undefined) renderRange(c, cfg[c.key]);
    }

    dom.monitorSelect.disabled = !cfg.monitor_enabled;
    updateNSModeButtons(cfg.ns_mode);
    updateGateMarker(cfg);
    syncToneSlider(cfg);

    if (cfg.vst_path !== undefined) dom.vstPathInput.value = cfg.vst_path || '';
    if (cfg.buffer_size !== undefined) dom.bufferSizeSelect.value = String(cfg.buffer_size);

    updateActivePreset();
}

function updateGateMarker(cfg) {
    const threshold = finiteOrNull(cfg.gate_threshold_db);
    dom.gateMarker.hidden = !cfg.gate_enabled || threshold === null;
    if (!dom.gateMarker.hidden) dom.gateMarker.style.left = `${dbToPercent(threshold)}%`;
}

function updateNSModeButtons(mode) {
    for (const [name, btn] of Object.entries(NS_BUTTONS)) setPressed(btn, name === mode);
}

function setNSMode(mode) {
    updateNSModeButtons(mode);
    sendConfigChange('ns_mode', mode);
}

// Easy-mode tone slider reflects the EQ shape; the tone steps are the same values applied on change.
function syncToneSlider(cfg) {
    let idx = 1;
    if (cfg.eq_enabled) {
        if (cfg.eq_low_gain_db > 2.0 && cfg.eq_high_gain_db < -1.0) idx = 0;
        else if (cfg.eq_low_gain_db < 0.0 && cfg.eq_high_gain_db > 2.0) idx = 2;
    }
    dom.easyToneSlider.value = idx;
    setText(dom.easyToneVal, TONE_STEPS[idx].label);
}

function applyToneStep(idx) {
    const step = TONE_STEPS[idx];
    if (!step) return;
    sendBulkConfig({
        eq_enabled: true,
        eq_low_gain_db: step.low,
        eq_mid_gain_db: step.mid,
        eq_high_gain_db: step.high,
    });
    applySettingsToUI(settings);
}

// --- Presets --------------------------------------------------------------
function valuesEqual(a, b) {
    if (typeof a === 'number' && typeof b === 'number') return Math.abs(a - b) <= FLOAT_TOLERANCE;
    return a === b;
}

function presetMatches(values) {
    const keys = Object.keys(values);
    return keys.length > 0 && keys.every((k) => valuesEqual(settings[k], values[k]));
}

function findActivePreset() {
    for (const name of Object.keys(presets.builtin)) {
        if (presetMatches(presets.builtin[name])) return `builtin:${name}`;
    }
    for (const name of Object.keys(presets.user)) {
        if (presetMatches(presets.user[name])) return `user:${name}`;
    }
    return null;
}

function updateActivePreset() {
    const active = findActivePreset();
    document.querySelectorAll('[data-preset]').forEach((btn) => {
        setPressed(btn, active === `builtin:${btn.dataset.preset}`);
    });
    dom.userPresetList.querySelectorAll('[data-user-preset]').forEach((btn) => {
        setPressed(btn, active === `user:${btn.dataset.userPreset}`);
    });
}

async function applyPreset(name) {
    try {
        const data = await api('/api/presets/apply', { method: 'POST', body: { name } });
        // Drop slider values queued before the preset landed, or they would overwrite it.
        throttles.forEach((slot) => { slot.pending = false; });
        settings = data && data.settings ? data.settings : await api('/api/settings');
        applySettingsToUI(settings);
        showToast(`"${BUILTIN_LABELS[name] || name}" preset applied.`);
    } catch (err) {
        console.error('Apply preset failed:', err);
        showToast(`Could not apply preset: ${err.message}`, 'error', 5000);
    }
}

function renderUserPresets() {
    disarmDelete();
    dom.userPresetList.replaceChildren();

    const names = Object.keys(presets.user).sort((a, b) => a.localeCompare(b));
    if (names.length === 0) {
        const empty = document.createElement('p');
        empty.className = 'presets-empty';
        empty.textContent = 'No saved presets yet.';
        dom.userPresetList.append(empty);
    }

    for (const name of names) {
        const chip = document.createElement('div');
        chip.className = 'preset-chip';

        const apply = document.createElement('button');
        apply.type = 'button';
        apply.className = 'chip-apply';
        apply.textContent = name;
        apply.title = `Apply "${name}"`;
        apply.dataset.userPreset = name;
        apply.addEventListener('click', () => applyPreset(name));

        const remove = document.createElement('button');
        remove.type = 'button';
        remove.className = 'chip-delete';
        remove.textContent = '×';
        remove.setAttribute('aria-label', `Delete preset ${name}`);
        remove.title = `Delete "${name}"`;
        remove.addEventListener('click', () => onDeleteClick(remove, name));

        chip.append(apply, remove);
        dom.userPresetList.append(chip);
    }

    updateActivePreset();
}

// Delete takes two clicks: the first arms the button, the second confirms.
function onDeleteClick(btn, name) {
    if (armedDelete && armedDelete.btn === btn) {
        const confirmed = armedDelete.name;
        disarmDelete();
        deleteUserPreset(confirmed);
        return;
    }
    disarmDelete();
    armedDelete = {
        btn,
        name,
        timer: setTimeout(disarmDelete, DELETE_ARM_MS),
    };
    btn.classList.add('armed');
    btn.textContent = 'Sure?';
    btn.setAttribute('aria-label', `Confirm delete preset ${name}`);
}

function disarmDelete() {
    if (!armedDelete) return;
    clearTimeout(armedDelete.timer);
    const { btn, name } = armedDelete;
    btn.classList.remove('armed');
    btn.textContent = '×';
    btn.setAttribute('aria-label', `Delete preset ${name}`);
    armedDelete = null;
}

async function deleteUserPreset(name) {
    try {
        await api(`/api/presets/${encodeURIComponent(name)}`, { method: 'DELETE' });
        showToast(`Preset "${name}" deleted.`);
        await fetchPresets();
    } catch (err) {
        console.error('Delete preset failed:', err);
        showToast(`Could not delete preset: ${err.message}`, 'error', 5000);
    }
}

async function saveCurrentAsPreset(event) {
    event.preventDefault();
    const name = dom.savePresetInput.value.trim();
    if (name.length < 1 || name.length > 32) {
        showToast('Preset names must be 1 to 32 characters.', 'error');
        dom.savePresetInput.focus();
        return;
    }

    dom.savePresetBtn.disabled = true;
    try {
        await api('/api/presets', { method: 'POST', body: { name } });
        dom.savePresetInput.value = '';
        showToast(`Preset "${name}" saved.`);
        await fetchPresets();
    } catch (err) {
        console.error('Save preset failed:', err);
        showToast(`Could not save preset: ${err.message}`, 'error', 5000);
    } finally {
        dom.savePresetBtn.disabled = false;
    }
}

// --- Engine, devices, VST actions -----------------------------------------
async function togglePower() {
    if (powerBusy) return;
    const starting = !engineRunning;
    powerBusy = true;
    dom.powerBtn.disabled = true;
    try {
        await api(starting ? '/api/start' : '/api/stop', { method: 'POST' });
        fetchSettings().catch((err) => console.error('Settings refresh failed:', err));
    } catch (err) {
        console.error('Engine toggle failed:', err);
        showToast(
            starting ? 'Could not start the audio engine. Check your audio devices.' : 'Could not stop the audio engine.',
            'error',
            5000
        );
    } finally {
        powerBusy = false;
        dom.powerBtn.disabled = false;
    }
}

async function refreshDevices() {
    dom.refreshDevicesBtn.classList.add('spinning');
    try {
        await fetchDevices();
    } catch (err) {
        console.error('Device refresh failed:', err);
        showToast('Could not refresh audio devices.', 'error');
    } finally {
        setTimeout(() => dom.refreshDevicesBtn.classList.remove('spinning'), 450);
    }
}

async function loadVst() {
    const path = dom.vstPathInput.value.trim();
    if (!path) {
        showToast('Enter the path to a .vst3 file first.', 'error');
        return;
    }
    sendConfigChange('vst_path', path);
    showToast('Loading VST3 plugin...', 'info', 2000);

    // The backend loads the plugin off the request path and returns no acknowledgement,
    // so the result is read back from settings after a short wait.
    await delay(1500);
    try {
        await fetchSettings();
    } catch (err) {
        console.error('Settings refresh failed:', err);
    }
    if (settings.vst_enabled && settings.vst_path && !isVstFailed) {
        showToast('VST3 plugin loaded.', 'success');
    } else {
        showToast('VST3 plugin failed to load. Check the file path.', 'error', 5000);
    }
}

async function openVstEditor() {
    try {
        await api('/api/vst/show-editor', { method: 'POST' });
    } catch (err) {
        console.error('Failed to show VST editor:', err);
        showToast('Could not open the plugin editor.', 'error');
    }
}

// --- Event wiring ---------------------------------------------------------
function setViewMode(mode) {
    dom.appWrapper.classList.toggle('mode-simple', mode === 'simple');
    dom.appWrapper.classList.toggle('mode-adv', mode === 'adv');
    setPressed(dom.viewModeSimple, mode === 'simple');
    setPressed(dom.viewModeAdv, mode === 'adv');
}

function setupEventListeners() {
    dom.viewModeSimple.addEventListener('click', () => setViewMode('simple'));
    dom.viewModeAdv.addEventListener('click', () => setViewMode('adv'));

    const fxButtons = Array.from(dom.fxTabs.querySelectorAll('.tab-btn'));
    fxButtons.forEach((btn) => {
        btn.addEventListener('click', () => {
            fxButtons.forEach((b) => setPressed(b, b === btn));
            document.querySelectorAll('.tab-panel').forEach((p) => {
                p.classList.toggle('active', p.id === btn.dataset.target);
            });
        });
    });

    dom.powerBtn.addEventListener('click', togglePower);
    dom.refreshDevicesBtn.addEventListener('click', refreshDevices);

    dom.inputSelect.addEventListener('change', (e) => sendConfigChange('input_device', e.target.value));
    dom.outputSelect.addEventListener('change', (e) => {
        sendConfigChange('output_device', e.target.value);
        updateRoutingWarning();
    });
    dom.monitorSelect.addEventListener('change', (e) => sendConfigChange('monitor_device', e.target.value));
    dom.bufferSizeSelect.addEventListener('change', (e) => {
        sendConfigChange('buffer_size', e.target.value);
        showToast('Buffer size takes effect after the engine restarts (STOP, then START).', 'info', 5000);
    });

    for (const c of CHECK_CONTROLS) {
        c.el.addEventListener('change', () => {
            sendConfigChange(c.key, c.el.checked);
            if (c.onChange) c.onChange();
        });
    }

    document.querySelectorAll('[data-preset]').forEach((btn) => {
        btn.addEventListener('click', () => applyPreset(btn.dataset.preset));
    });
    dom.savePresetForm.addEventListener('submit', saveCurrentAsPreset);
    dom.easyToneSlider.addEventListener('input', (e) => applyToneStep(Number(e.target.value)));

    for (const c of RANGE_CONTROLS) bindRange(c);

    dom.nsModeOff.addEventListener('click', () => setNSMode('off'));
    dom.nsModeEco.addEventListener('click', () => setNSMode('eco'));
    dom.nsModeHigh.addEventListener('click', () => setNSMode('high'));

    dom.vstLoadBtn.addEventListener('click', loadVst);
    dom.vstPathInput.addEventListener('keydown', (e) => {
        if (e.key === 'Enter') loadVst();
    });
    dom.vstEditorBtn.addEventListener('click', openVstEditor);

    dom.vizTabSpec.addEventListener('click', () => setVizMode('spec'));
    dom.vizTabPulse.addEventListener('click', () => setVizMode('pulse'));
    dom.vizTabCascade.addEventListener('click', () => setVizMode('cascade'));
}

// --- Start ----------------------------------------------------------------
function init() {
    setupEventListeners();
    connectWebSocket();
    startVisualizer();
    loadAll();
}

init();
