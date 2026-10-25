//! Differences between the current plugin set and `bus.activation`.

#[derive(Debug, Default)]
pub struct ActivationDiff {
    pub enable: Vec<String>,
    pub disable: Vec<String>,
}

pub fn diff(
    all: &[String],
    enabled: &[String],
    disabled_by_activation: &[String],
    bootstrap: &[String],
) -> ActivationDiff {
    let mut out = ActivationDiff::default();
    for name in all {
        if bootstrap.iter().any(|item| item == name) {
            continue;
        }
        let should_disable = disabled_by_activation.iter().any(|item| item == name);
        let is_enabled = enabled.iter().any(|item| item == name);
        if should_disable && is_enabled {
            out.disable.push(name.clone());
        }
        if !should_disable && !is_enabled {
            out.enable.push(name.clone());
        }
    }
    out
}
