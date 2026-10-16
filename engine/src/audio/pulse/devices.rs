//! Device selection over the server's sink and source lists. Pure logic: no server is needed to test it.

use anyhow::{anyhow, Result};

use super::connection::Node;
use crate::audio::core::names::{default_input_index, default_output_index, is_hushcore_microphone, match_device, strip_host_suffix};
use crate::audio::core::DeviceSummary;

/// Minimum period the engine uses on this backend (see `streams::CAPTURE_PERIOD_MS`).
pub const MIN_PERIOD_MS: f32 = 5.0;

/// Inputs as the UI lists them: non-monitor sources, by description.
pub fn input_summaries(sources: &[Node]) -> Vec<DeviceSummary> {
    sources.iter().filter(|n| !n.is_monitor).map(summary).collect()
}

/// Outputs as the UI lists them: every sink, by description.
pub fn output_summaries(sinks: &[Node]) -> Vec<DeviceSummary> {
    sinks.iter().map(summary).collect()
}

fn summary(node: &Node) -> DeviceSummary {
    DeviceSummary {
        name: node.description.clone(),
        is_default: node.is_default,
        min_period_ms: MIN_PERIOD_MS,
        sample_rate: node.rate,
    }
}

/// Picks the output for `requested`. An empty request means HushCore's sink when installed (only when
/// `prefer_virtual`), else the server default. A non-empty request matches the exact pulse name first,
/// then the UI name the same way the other backends do.
pub fn resolve_output<'a>(sinks: &'a [Node], requested: &str, prefer_virtual: bool) -> Result<&'a Node> {
    let wanted = strip_host_suffix(requested);
    let descriptions: Vec<String> = sinks.iter().map(|n| n.description.clone()).collect();
    if wanted.is_empty() {
        let default_index = sinks.iter().position(|n| n.is_default);
        let index = if prefer_virtual { default_output_index(&descriptions, default_index) } else { default_index };
        return index.map(|i| &sinks[i]).ok_or_else(|| anyhow!("no default output device"));
    }
    if let Some(node) = sinks.iter().find(|n| n.pulse_name == wanted) {
        return Ok(node);
    }
    match_device(wanted, &descriptions)
        .map(|i| &sinks[i])
        .ok_or_else(|| anyhow!("no output device matches \"{wanted}\""))
}

/// Picks the input for `requested`. An empty request means the server default source, unless that is
/// HushCore's microphone (a feedback loop), then the first other real source. Monitor sources are never
/// chosen by an empty request unless the user made one the default. A non-empty request matches the
/// exact pulse name, then the UI name among real sources, then an exact monitor name or description.
pub fn resolve_input<'a>(sources: &'a [Node], requested: &str) -> Result<&'a Node> {
    let wanted = strip_host_suffix(requested);
    if wanted.is_empty() {
        if let Some(node) = sources.iter().find(|n| n.is_default && n.is_monitor && !is_hushcore_microphone(&n.description)) {
            return Ok(node);
        }
        let real: Vec<&Node> = sources.iter().filter(|n| !n.is_monitor).collect();
        let descriptions: Vec<String> = real.iter().map(|n| n.description.clone()).collect();
        let default_index = real.iter().position(|n| n.is_default);
        return default_input_index(&descriptions, default_index)
            .map(|i| real[i])
            .ok_or_else(|| anyhow!("no usable input device (HushCore Microphone is never captured by default)"));
    }
    if let Some(node) = sources.iter().find(|n| n.pulse_name == wanted) {
        return Ok(node);
    }
    let real: Vec<&Node> = sources.iter().filter(|n| !n.is_monitor).collect();
    let descriptions: Vec<String> = real.iter().map(|n| n.description.clone()).collect();
    if let Some(i) = match_device(wanted, &descriptions) {
        return Ok(real[i]);
    }
    sources
        .iter()
        .filter(|n| n.is_monitor)
        .find(|n| n.pulse_name.eq_ignore_ascii_case(wanted) || n.description.eq_ignore_ascii_case(wanted))
        .ok_or_else(|| anyhow!("no input device matches \"{wanted}\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(pulse: &str, desc: &str, default: bool, monitor: bool) -> Node {
        Node {
            pulse_name: pulse.into(),
            description: desc.into(),
            rate: 48_000,
            owner_module: None,
            is_monitor: monitor,
            is_default: default,
        }
    }

    #[test]
    fn empty_output_prefers_the_hushcore_sink() {
        let sinks = [node("alsa", "Speakers", true, false), node("hushcore_sink", "HushCore Sink", false, false)];
        assert_eq!(resolve_output(&sinks, "", true).unwrap().pulse_name, "hushcore_sink");
        assert_eq!(resolve_output(&sinks, "", false).unwrap().pulse_name, "alsa");
    }

    #[test]
    fn output_accepts_pulse_name_then_description() {
        let sinks = [node("alsa", "Speakers", true, false), node("hushcore_sink", "HushCore Sink", false, false)];
        assert_eq!(resolve_output(&sinks, "hushcore_sink", false).unwrap().pulse_name, "hushcore_sink");
        assert_eq!(resolve_output(&sinks, "speakers [Linux PulseAudio]", false).unwrap().pulse_name, "alsa");
        assert!(resolve_output(&sinks, "nope", false).is_err());
    }

    #[test]
    fn empty_input_never_captures_the_hushcore_microphone() {
        let sources = [
            node("hushcore_mic", "HushCore Microphone", true, false),
            node("mic", "USB Mic", false, false),
        ];
        assert_eq!(resolve_input(&sources, "").unwrap().pulse_name, "mic");
    }

    #[test]
    fn empty_input_uses_a_monitor_only_when_it_is_the_default() {
        let sources = [node("real", "Built-in", false, false), node("fake.monitor", "Monitor of fake", true, true)];
        assert_eq!(resolve_input(&sources, "").unwrap().pulse_name, "fake.monitor");
        let plain = [node("real", "Built-in", true, false), node("fake.monitor", "Monitor of fake", false, true)];
        assert_eq!(resolve_input(&plain, "").unwrap().pulse_name, "real");
    }

    #[test]
    fn explicit_monitor_request_resolves_by_exact_name_or_description() {
        let sources = [node("real", "Built-in", true, false), node("fake.monitor", "Monitor of fake", false, true)];
        assert_eq!(resolve_input(&sources, "fake.monitor").unwrap().pulse_name, "fake.monitor");
        assert_eq!(resolve_input(&sources, "monitor of fake").unwrap().pulse_name, "fake.monitor");
        assert!(resolve_input(&sources, "fake").is_err());
    }

    #[test]
    fn input_summaries_hide_monitors() {
        let sources = [node("real", "Built-in", true, false), node("fake.monitor", "Monitor of fake", false, true)];
        let list = input_summaries(&sources);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "Built-in");
        assert!(list[0].is_default);
    }
}
