//! The pre-flight check of `cargo xtask bench`: what the machine does to the
//! clock of the core being measured. Turbo boost decays as the machine heats
//! up, so a long run drifts by several percent; a busy hyperthread sibling
//! shares the core. The state is recorded with every measurement, so numbers
//! taken under different settings are never compared by accident.
//!
//! Everything is read from Linux's sysfs; on another system, or where a file
//! is missing, the setting is recorded as unknown and nothing is warned about.

use std::fs;

use serde_json::{Value, json};

const CPU_DIR: &str = "/sys/devices/system/cpu";

/// The clock settings that decide how far two runs can be compared.
pub(crate) struct Machine {
    /// The frequency governor of the measured core, e.g. `performance`.
    governor: Option<String>,
    is_turbo_on: Option<bool>,
    /// The other hardware threads of the measured core that are online.
    online_siblings: Vec<String>,
}

impl Machine {
    /// Read the settings for `cpu`, the core the benches are pinned to.
    /// Unpinned, the governor is read from CPU 0 and siblings are not checked.
    pub(crate) fn inspect(cpu: Option<&str>) -> Machine {
        let governor = read(&format!(
            "{CPU_DIR}/cpu{}/cpufreq/scaling_governor",
            cpu.unwrap_or("0")
        ));
        Machine {
            governor,
            is_turbo_on: is_turbo_on(),
            online_siblings: cpu.map_or_else(Vec::new, online_siblings),
        }
    }

    /// One line per setting that makes the numbers drift, with its remedy.
    pub(crate) fn warnings(&self, cpu: Option<&str>) -> Vec<String> {
        let mut warnings = Vec::new();
        if cpu.is_none() {
            warnings.push("not pinned to a core: pass --cpu N".to_string());
        }
        if self.is_turbo_on == Some(true) {
            warnings.push(
                "turbo boost is on, so the clock falls as the machine heats up: \
                 echo 1 | sudo tee /sys/devices/system/cpu/intel_pstate/no_turbo \
                 (or echo 0 into cpufreq/boost)"
                    .to_string(),
            );
        }
        if let Some(governor) = &self.governor
            && governor != "performance"
        {
            warnings.push(format!(
                "the frequency governor is {governor:?}, not \"performance\": \
                 sudo cpupower frequency-set -g performance"
            ));
        }
        for sibling in &self.online_siblings {
            warnings.push(format!(
                "CPU {sibling} shares the measured core and is online: \
                 echo 0 | sudo tee {CPU_DIR}/cpu{sibling}/online"
            ));
        }
        warnings
    }

    /// The settings as recorded in a `bench-history` file.
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "governor": self.governor,
            "turbo": self.is_turbo_on,
            "online_siblings": self.online_siblings,
        })
    }
}

/// Intel's driver reports `no_turbo`; the generic one reports `boost`.
fn is_turbo_on() -> Option<bool> {
    if let Some(no_turbo) = read(&format!("{CPU_DIR}/intel_pstate/no_turbo")) {
        return Some(no_turbo == "0");
    }
    read(&format!("{CPU_DIR}/cpufreq/boost")).map(|boost| boost == "1")
}

fn online_siblings(cpu: &str) -> Vec<String> {
    let Some(list) = read(&format!("{CPU_DIR}/cpu{cpu}/topology/thread_siblings_list")) else {
        return Vec::new();
    };
    parse_cpu_list(&list)
        .into_iter()
        .filter(|sibling| sibling != cpu)
        .collect()
}

/// Expand a sysfs CPU list such as `6-7` or `0,4-5` into its members. The
/// siblings list names only the threads that are online.
fn parse_cpu_list(list: &str) -> Vec<String> {
    list.split(',')
        .flat_map(|part| {
            let (first, last) = part
                .split_once('-')
                .unwrap_or((part, part));
            let first: u32 = first
                .trim()
                .parse()
                .unwrap_or(0);
            let last: u32 = last
                .trim()
                .parse()
                .unwrap_or(first);
            first..=last
        })
        .map(|cpu| cpu.to_string())
        .collect()
}

fn read(path: &str) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|text| {
            text.trim()
                .to_string()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cpu_list_expands_ranges_and_single_cpus() {
        assert_eq!(parse_cpu_list("6-7"), ["6", "7"]);
        assert_eq!(parse_cpu_list("12"), ["12"]);
        assert_eq!(parse_cpu_list("0,4-6"), ["0", "4", "5", "6"]);
    }

    #[test]
    fn a_quiet_pinned_machine_has_no_warnings() {
        let machine = Machine {
            governor: Some("performance".to_string()),
            is_turbo_on: Some(false),
            online_siblings: Vec::new(),
        };
        assert!(
            machine
                .warnings(Some("6"))
                .is_empty()
        );
    }

    #[test]
    fn each_drifting_setting_is_warned_about() {
        let machine = Machine {
            governor: Some("powersave".to_string()),
            is_turbo_on: Some(true),
            online_siblings: vec!["7".to_string()],
        };
        assert_eq!(
            machine
                .warnings(None)
                .len(),
            4
        );
    }

    #[test]
    fn unknown_settings_are_not_warned_about() {
        let machine = Machine {
            governor: None,
            is_turbo_on: None,
            online_siblings: Vec::new(),
        };
        assert!(
            machine
                .warnings(Some("0"))
                .is_empty()
        );
    }
}
