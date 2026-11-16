use std::fs;

pub fn inspect_cpu_topology_json() -> String {
    let mut freqs = Vec::new();
    for cpu in 0..16 {
        let path = format!("/sys/devices/system/cpu/cpu{}/cpufreq/cpuinfo_max_freq", cpu);
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(khz) = raw.trim().parse::<u64>() {
                freqs.push((cpu, khz));
            }
        }
    }
    if freqs.is_empty() {
        return r#"{"topology":"generic-arm64","cores":8}"#.to_string();
    }
    let max_khz = freqs.iter().map(|(_, f)| *f).max().unwrap_or(0);
    format!(
        r#"{{"detected_cores":{},"prime_max_khz":{}}}"#,
        freqs.len(),
        max_khz
    )
}

pub fn apply_runtime_tuning(
    snapdragon_pinning: bool,
    io_prio_be: bool,
    thermal_guard_c: i32,
) -> String {
    format!(
        "pinning={} ioprio_be={} thermal_guard={}C",
        snapdragon_pinning, io_prio_be, thermal_guard_c
    )
}
