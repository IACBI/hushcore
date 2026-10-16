//! Device-name matching rules from PROTOCOL.md. Pure logic, so it is tested without audio hardware.

/// Render endpoint the engine writes to. Every platform's virtual device uses this name (on Windows the
/// endpoint name is followed by the adapter name in parentheses, so matching is by substring).
pub const SINK_NAME: &str = "HushCore Sink";
/// Capture endpoint other apps read the processed voice from.
pub const MICROPHONE_NAME: &str = "HushCore Microphone";

/// Removes a trailing ` [host API]` suffix that the UI appends to device names.
pub fn strip_host_suffix(name: &str) -> &str {
    let trimmed = name.trim();
    if let Some(body) = trimmed.strip_suffix(']') {
        if let Some(idx) = body.rfind(" [") {
            return body[..idx].trim_end();
        }
    }
    trimmed
}

/// Index of the device matching `requested`: case-insensitive equality first, then substring.
/// An empty request matches nothing; the caller decides what "default" means.
pub fn match_device(requested: &str, names: &[String]) -> Option<usize> {
    let want = strip_host_suffix(requested).to_lowercase();
    if want.is_empty() {
        return None;
    }
    let lowered: Vec<String> = names.iter().map(|n| strip_host_suffix(n).to_lowercase()).collect();
    lowered
        .iter()
        .position(|n| *n == want)
        .or_else(|| lowered.iter().position(|n| n.contains(&want)))
}

fn contains_ignore_case(name: &str, needle: &str) -> bool {
    name.to_lowercase().contains(&needle.to_lowercase())
}

/// True for HushCore's own virtual sink.
pub fn is_hushcore_sink(name: &str) -> bool {
    contains_ignore_case(name, SINK_NAME)
}

/// True for HushCore's own virtual microphone. The engine never captures from it by default, which
/// would feed its own output back into its input.
pub fn is_hushcore_microphone(name: &str) -> bool {
    contains_ignore_case(name, MICROPHONE_NAME)
}

/// Output picked when the request is empty: the HushCore sink if installed, else the default endpoint.
pub fn default_output_index(names: &[String], default_index: Option<usize>) -> Option<usize> {
    names.iter().position(|n| is_hushcore_sink(n)).or(default_index)
}

/// Input picked when the request is empty: the default endpoint, unless that is the HushCore
/// microphone (a feedback loop), in which case the first other input.
pub fn default_input_index(names: &[String], default_index: Option<usize>) -> Option<usize> {
    match default_index {
        Some(i) if names.get(i).is_some_and(|n| !is_hushcore_microphone(n)) => Some(i),
        _ => names.iter().position(|n| !is_hushcore_microphone(n)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn strips_trailing_host_api_suffix_only() {
        assert_eq!(
            strip_host_suffix("Mikrofon (Realtek(R) Audio) [Windows WASAPI]"),
            "Mikrofon (Realtek(R) Audio)"
        );
        assert_eq!(strip_host_suffix("Speakers"), "Speakers");
        assert_eq!(strip_host_suffix("Odd [x] name"), "Odd [x] name");
        assert_eq!(strip_host_suffix("  Spaced [MME]  "), "Spaced");
    }

    #[test]
    fn equality_is_case_insensitive_and_beats_substring() {
        let list = names(&["Speakers (Realtek)", "Speakers"]);
        assert_eq!(match_device("speakers", &list), Some(1));
        assert_eq!(match_device("SPEAKERS (REALTEK) [Windows WASAPI]", &list), Some(0));
    }

    #[test]
    fn falls_back_to_substring() {
        let list = names(&["Microphone Array (Intel)", "HushCore Microphone (HushCore Virtual Audio)"]);
        assert_eq!(match_device("hushcore microphone", &list), Some(1));
        assert_eq!(match_device("nope", &list), None);
    }

    #[test]
    fn empty_request_matches_nothing() {
        assert_eq!(match_device("   ", &names(&["A"])), None);
        assert_eq!(match_device(" [Windows WASAPI]", &names(&["A"])), None);
    }

    #[test]
    fn output_default_prefers_hushcore_sink() {
        let list = names(&["Speakers", "HushCore Sink (HushCore Virtual Audio)"]);
        assert_eq!(default_output_index(&list, Some(0)), Some(1));
        let plain = names(&["Speakers", "Headphones"]);
        assert_eq!(default_output_index(&plain, Some(1)), Some(1));
        assert_eq!(default_output_index(&plain, None), None);
    }

    #[test]
    fn input_default_never_picks_hushcore_microphone() {
        let list = names(&["HushCore Microphone (HushCore Virtual Audio)", "Mic (USB)"]);
        assert_eq!(default_input_index(&list, Some(0)), Some(1));
        assert_eq!(default_input_index(&list, Some(1)), Some(1));
        assert_eq!(default_input_index(&list, None), Some(1));
        assert_eq!(default_input_index(&names(&["HushCore Microphone"]), Some(0)), None);
    }
}
