//! The HushCore virtual pair on the PulseAudio protocol. A persistent PipeWire or PulseAudio config
//! provides it by name and it is reused. When it is absent and the engine is asked for the default
//! output, this process loads it on demand and unloads exactly those modules when it exits.

use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};

use super::connection::{Node, Server, ServerInfo};
use crate::audio::core::names::{is_hushcore_microphone, is_hushcore_sink};

pub const SINK_PULSE_NAME: &str = "hushcore_sink";
pub const MIC_PULSE_NAME: &str = "hushcore_mic";
const SINK_MODULE: &str = "module-null-sink";
// The description is a quoted proplist value: an unquoted space ends the value, and the server
// then keeps only the first word ("HushCore").
const SINK_ARGS: &str = r#"sink_name=hushcore_sink sink_properties="device.description=\"HushCore Sink\"""#;
const MIC_MODULE: &str = "module-remap-source";
const MIC_ARGS: &str = r#"source_name=hushcore_mic master=hushcore_sink.monitor source_properties="device.description=\"HushCore Microphone\"""#;
const APPEAR_TIMEOUT: Duration = Duration::from_secs(3);

/// Modules this process loaded. Only these are unloaded on exit, never a pair found by name.
static OWNED: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Per-process lifetime of the backend. Dropping it (process exit, end of `serve`) unloads the
/// on-demand modules this process loaded.
pub struct PlatformInit;

impl PlatformInit {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }
}

impl Drop for PlatformInit {
    fn drop(&mut self) {
        unload_owned();
    }
}

/// Unloads the modules this process loaded. Best effort: the server may already be gone.
fn unload_owned() {
    let indices = match OWNED.lock() {
        Ok(mut owned) => std::mem::take(&mut *owned),
        Err(_) => return,
    };
    if indices.is_empty() {
        return;
    }
    if let Ok(mut server) = Server::connect() {
        for index in indices {
            let _ = server.unload_module(index);
        }
    }
}

/// True when the module at `index` was loaded by this process.
pub fn is_owned(index: Option<u32>) -> bool {
    match (index, OWNED.lock()) {
        (Some(index), Ok(owned)) => owned.contains(&index),
        _ => false,
    }
}

/// Loads whichever half of the pair is missing and waits until both sinks and sources list it.
/// Returns true when this call loaded something.
pub fn ensure_pair(server: &mut Server, info: &ServerInfo) -> Result<bool> {
    let sinks = server.sinks(info)?;
    let sources = server.sources(info)?;
    let have_sink = sinks.iter().any(|n| n.pulse_name == SINK_PULSE_NAME || is_hushcore_sink(&n.description));
    let have_mic = sources.iter().any(|n| n.pulse_name == MIC_PULSE_NAME || is_hushcore_microphone(&n.description));
    if have_sink && have_mic {
        return Ok(false);
    }
    // The remap source needs its master sink, so a missing sink is always loaded first.
    if !have_sink {
        let index = server.load_module(SINK_MODULE, SINK_ARGS).context("cannot create HushCore Sink")?;
        remember(index);
    }
    if !have_mic {
        let index = server.load_module(MIC_MODULE, MIC_ARGS).context("cannot create HushCore Microphone")?;
        remember(index);
    }
    wait_for_pair(server, info)?;
    Ok(true)
}

fn remember(index: u32) {
    if let Ok(mut owned) = OWNED.lock() {
        owned.push(index);
    }
}

fn wait_for_pair(server: &mut Server, info: &ServerInfo) -> Result<()> {
    let deadline = Instant::now() + APPEAR_TIMEOUT;
    loop {
        let sinks = server.sinks(info)?;
        let sources = server.sources(info)?;
        let ready = sinks.iter().any(|n| is_hushcore_sink(&n.description))
            && sources.iter().any(|n| is_hushcore_microphone(&n.description));
        if ready || Instant::now() >= deadline {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Which component provides the pair: `on-demand` when this process loaded the sink, else the server.
pub fn provider(info: &ServerInfo, sink: Option<&Node>) -> &'static str {
    if is_owned(sink.and_then(|n| n.owner_module)) {
        "on-demand"
    } else if info.server_name.to_ascii_lowercase().contains("pipewire") {
        "pipewire"
    } else {
        "pulseaudio"
    }
}
