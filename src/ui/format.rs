pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    if bytes < 1024 {
        return format!("{bytes}B");
    }
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024 * 1024 && unit < UNITS.len() - 2 {
        value /= 1024;
        unit += 1;
    }
    let whole = value / 1024;
    let frac = (value % 1024) * 10 / 1024;
    unit += 1;
    if whole >= 100 {
        format!("{whole}{}", UNITS[unit])
    } else {
        format!("{whole}.{frac}{}", UNITS[unit])
    }
}

pub fn format_rate(bytes_per_sec: u64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec))
}

pub fn format_duration(seconds: u32) -> String {
    let minutes = seconds / 60;
    let remainder = seconds % 60;
    format!("{minutes}:{remainder:02}")
}

pub fn peak(samples: &[u64]) -> u64 {
    samples.iter().copied().max().unwrap_or(0)
}

pub fn latest(samples: &[u64]) -> u64 {
    samples.last().copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_units() {
        assert_eq!(format_bytes(0), "0B");
        assert_eq!(format_bytes(1023), "1023B");
        assert_eq!(format_bytes(1536), "1.5K");
        assert_eq!(format_bytes(1_048_576), "1.0M");
        assert_eq!(format_rate(1536), "1.5K/s");
    }

    #[test]
    fn duration_is_minutes_and_seconds() {
        assert_eq!(format_duration(562), "9:22");
    }
}
