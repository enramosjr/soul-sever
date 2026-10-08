//! Spectral quality of one audio file.
//!
//! The measurement reads samples and does not write the file. A write would change
//! the modification time and make the library scan read tags again.

use std::fs::File;
use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as AudioError;
use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

use crate::model::{ContainerClass, SearchHit, container_class};

/// Samples in one analysis block.
const WINDOW: usize = 16_384;
/// Sought positions, at 10% through 80% of the container.
const WINDOWS: usize = 8;
/// A block quieter than this is silence, not a codec wall.
const SILENCE_AMPLITUDE: f32 = 0.003_162_277_6;
/// Bins this far below the block peak do not count as content.
const FLOOR_RATIO: f64 = 0.001;
/// A lossless container whose energy reaches this close to Nyquist is truly lossless.
pub const LOSSLESS_MAX_PERCENT: u8 = 10;
/// Remote duration may differ by this many seconds and still be the same song.
pub const DURATION_TOLERANCE_SECS: u32 = 3;
/// Stop a cursory probe after this many hits.
pub const PROBE_HITS: usize = 40;

/// What the spectrum and the container say about a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Lossless,
    Lossy,
    Unmeasured,
}

/// Saved next to the song. Absent fields in an older cache load as defaults.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Quality {
    pub bit_depth: u32,
    pub sample_rate: u32,
    pub lossy_percent: Option<u8>,
    pub cutoff_hz: u32,
    pub duration_secs: Option<u32>,
    pub verdict: Verdict,
    #[serde(default)]
    pub upgrade_attempted: bool,
}

/// Which replacement a verdict is allowed to look for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpgradeGoal {
    /// Truly lossless 16-bit: only a 24-bit file.
    TwentyFour,
    /// Not truly lossless: any truly lossless file.
    AnyLossless,
}

impl Quality {
    /// Twice the measured cutoff. A 96 kHz file that stops at 22 kHz does not
    /// outrank a full-band 44.1 kHz file.
    pub fn real_rate_hz(&self) -> u32 {
        self.cutoff_hz.saturating_mul(2)
    }

    /// Bit depth, container rate, and the track-panel line.
    pub fn facts(&self) -> String {
        format!(
            "{} · {} · {}",
            depth_label(self.bit_depth),
            rate_label(self.sample_rate),
            self.row()
        )
    }

    /// The track-panel line. An unmeasured file has no percent.
    pub fn row(&self) -> String {
        match self.verdict {
            Verdict::Unmeasured => "not measured".to_owned(),
            Verdict::Lossless => {
                format!("lossless · {}% lossy", self.lossy_percent.unwrap_or(0))
            }
            Verdict::Lossy => match self.lossy_percent {
                Some(percent) => format!("lossy · {percent}%"),
                None => "lossy".to_owned(),
            },
        }
    }

    /// `None` when this file should be kept. A search already tried for this stamp
    /// is not tried again.
    pub fn goal(&self) -> Option<UpgradeGoal> {
        if self.upgrade_attempted {
            return None;
        }
        match self.verdict {
            Verdict::Lossless if self.bit_depth >= 24 => None,
            Verdict::Lossless => Some(UpgradeGoal::TwentyFour),
            Verdict::Lossy => Some(UpgradeGoal::AnyLossless),
            Verdict::Unmeasured => None,
        }
    }
}

pub fn depth_label(bits: u32) -> String {
    if bits == 0 {
        "depth unknown".to_owned()
    } else {
        format!("{bits}-bit")
    }
}

pub fn rate_label(rate: u32) -> String {
    if rate == 0 {
        return "rate unknown".to_owned();
    }
    let khz = rate / 1000;
    let tenth = (rate % 1000) / 100;
    if tenth == 0 {
        format!("{khz} kHz")
    } else {
        format!("{khz}.{tenth} kHz")
    }
}

/// Highest frequency that still carries energy, over every full block in `samples`.
/// Silence does not set a cutoff.
pub fn cutoff_hz(samples: &[f32], sample_rate: u32) -> Option<f64> {
    if sample_rate == 0 {
        return None;
    }
    let mut best: Option<f64> = None;
    for block in samples.chunks_exact(WINDOW) {
        if let Some(hz) = block_cutoff(block, sample_rate) {
            best = Some(best.map_or(hz, |prev| prev.max(hz)));
        }
    }
    best
}

/// `round(100 * (nyquist - cutoff) / nyquist)`, clamped to 0–100.
pub fn lossy_percent(cutoff_hz: f64, sample_rate: u32) -> u8 {
    let nyquist = f64::from(sample_rate) / 2.0;
    if nyquist <= 0.0 {
        return 100;
    }
    let missing = ((nyquist - cutoff_hz) / nyquist).clamp(0.0, 1.0);
    missing.mul_add(100.0, 0.0).round().clamp(0.0, 100.0) as u8
}

/// Bin with the strongest magnitude. Used to check the transform against a known sine.
pub fn dominant_bin(samples: &[f32]) -> Option<usize> {
    let n = samples.len();
    if n < 2 || !n.is_power_of_two() {
        return None;
    }
    let mut re = vec![0.0; n];
    let mut im = vec![0.0; n];
    for (index, sample) in samples.iter().enumerate() {
        re[index] = f64::from(*sample) * hann(index, n);
    }
    fft(&mut re, &mut im);
    let mut best = 1usize;
    let mut peak = 0.0;
    for bin in 1..n / 2 {
        let mag = magnitude(re[bin], im[bin]);
        if mag > peak {
            peak = mag;
            best = bin;
        }
    }
    (peak > 0.0).then_some(best)
}

/// Read `path` and judge it. A decode error on a known lossy extension is lossy
/// with no percent. A known lossless extension that will not decode is left unmeasured.
pub fn analyze_path(path: &Path) -> Quality {
    #[cfg(test)]
    note_thread();
    #[cfg(test)]
    wait_if_paused();
    match measure_file(path) {
        Ok(measured) => judge(path, measured),
        Err(()) => undecoded(path),
    }
}

/// A lossy original accepts any truly lossless file. A truly lossless 16-bit
/// original accepts only 24-bit or higher whose real rate is at least as high.
pub fn accepts_replacement(original: &Quality, candidate: &Quality) -> bool {
    if candidate.verdict != Verdict::Lossless {
        return false;
    }
    match original.verdict {
        Verdict::Lossy => true,
        Verdict::Lossless if original.bit_depth < 24 => {
            candidate.bit_depth >= 24 && candidate.real_rate_hz() >= original.real_rate_hz()
        }
        _ => false,
    }
}

/// The best eligible hit. 24-bit outranks 16-bit, then a reported sample rate
/// outranks an omitted one, then the higher rate.
pub fn pick_upgrade<'a>(
    hits: &'a [SearchHit],
    goal: UpgradeGoal,
    artist: &str,
    title: &str,
    duration_secs: Option<u32>,
) -> Option<&'a SearchHit> {
    pick_upgrade_except(hits, goal, artist, title, duration_secs, &[])
}

/// [`pick_upgrade`] that skips users who already have a replacement download.
pub fn pick_upgrade_except<'a>(
    hits: &'a [SearchHit],
    goal: UpgradeGoal,
    artist: &str,
    title: &str,
    duration_secs: Option<u32>,
    busy: &[&str],
) -> Option<&'a SearchHit> {
    hits.iter()
        .filter(|hit| {
            eligible(hit, goal, artist, title, duration_secs) && !busy.contains(&hit.user.as_str())
        })
        .max_by_key(|hit| rank_key(hit))
}

/// Verify `staged`, file it under `root`, and delete `original` only after that
/// file is in place. A rejection deletes `staged` and leaves `original`.
pub fn replace_with(
    root: &Path,
    original_path: &Path,
    original: &Quality,
    staged: &Path,
) -> Option<std::path::PathBuf> {
    let candidate = analyze_path(staged);
    if !accepts_replacement(original, &candidate) {
        let _ = std::fs::remove_file(staged);
        return None;
    }
    let Some(dest) = crate::library::upgrade_destination(root, original_path, staged) else {
        let _ = std::fs::remove_file(staged);
        return None;
    };
    if !place_upgrade(root, original_path, staged, &dest) {
        return None;
    }
    Some(dest)
}

fn place_upgrade(root: &Path, original: &Path, staged: &Path, dest: &Path) -> bool {
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if dest == original {
        let file_name = dest
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("song");
        let tmp = dest.with_file_name(format!(".{file_name}.upgrade"));
        if std::fs::rename(staged, &tmp).is_err() {
            let _ = std::fs::remove_file(staged);
            return false;
        }
        if std::fs::rename(&tmp, dest).is_err() || !dest.is_file() {
            let _ = std::fs::remove_file(&tmp);
            return false;
        }
        return true;
    }
    if std::fs::rename(staged, dest).is_err() || !dest.is_file() {
        let _ = std::fs::remove_file(staged);
        let _ = std::fs::remove_file(dest);
        return false;
    }
    crate::library::discard(root, &[original.to_path_buf()]);
    if original.exists() {
        let _ = std::fs::remove_file(dest);
        return false;
    }
    true
}

fn eligible(
    hit: &SearchHit,
    goal: UpgradeGoal,
    artist: &str,
    title: &str,
    duration_secs: Option<u32>,
) -> bool {
    if !phrase_in(&hit.path, artist) || !phrase_in(&hit.path, title) {
        return false;
    }
    if let (Some(local), Some(remote)) = (duration_secs, hit.duration)
        && local.abs_diff(remote) > DURATION_TOLERANCE_SECS
    {
        return false;
    }
    let class = container_class(hit.extension(), hit.bit_depth);
    if class != ContainerClass::Lossless {
        return false;
    }
    match goal {
        UpgradeGoal::TwentyFour => hit.bit_depth.unwrap_or(0) >= 24,
        UpgradeGoal::AnyLossless => true,
    }
}

fn rank_key(hit: &SearchHit) -> (u32, bool, u32, bool, u32) {
    (
        hit.bit_depth.unwrap_or(0),
        hit.sample_rate.is_some(),
        hit.sample_rate.unwrap_or(0),
        hit.free_slot,
        u32::MAX - hit.queue,
    )
}

/// `phrase` is in `path` when its words sit together, in order. `Air` does not
/// match `Airborne`, and `Song` does not match `Songbird`.
fn phrase_in(path: &str, phrase: &str) -> bool {
    let needle = words(phrase);
    if needle.is_empty() {
        return false;
    }
    let hay = words(path);
    hay.windows(needle.len()).any(|window| window == needle)
}

fn words(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| part.to_lowercase())
        .collect()
}

struct Measured {
    bit_depth: u32,
    sample_rate: u32,
    duration_secs: Option<u32>,
    cutoff_hz: Option<f64>,
}

fn judge(path: &Path, measured: Measured) -> Quality {
    let depth = (measured.bit_depth > 0).then_some(measured.bit_depth);
    let class = container_class(path_extension(path), depth);
    let percent = measured
        .cutoff_hz
        .map(|hz| lossy_percent(hz, measured.sample_rate));
    let verdict = match class {
        ContainerClass::Lossless => match percent {
            Some(percent) if percent <= LOSSLESS_MAX_PERCENT => Verdict::Lossless,
            Some(_) => Verdict::Lossy,
            None => Verdict::Unmeasured,
        },
        ContainerClass::Lossy => Verdict::Lossy,
        ContainerClass::Other => Verdict::Unmeasured,
    };
    Quality {
        bit_depth: measured.bit_depth,
        sample_rate: measured.sample_rate,
        lossy_percent: percent,
        cutoff_hz: measured
            .cutoff_hz
            .map(|hz| hz.round().clamp(0.0, f64::from(u32::MAX)) as u32)
            .unwrap_or(0),
        duration_secs: measured.duration_secs,
        verdict,
        upgrade_attempted: false,
    }
}

fn undecoded(path: &Path) -> Quality {
    let verdict = match container_class(path_extension(path), None) {
        ContainerClass::Lossy => Verdict::Lossy,
        ContainerClass::Lossless | ContainerClass::Other => Verdict::Unmeasured,
    };
    Quality {
        bit_depth: 0,
        sample_rate: 0,
        lossy_percent: None,
        cutoff_hz: 0,
        duration_secs: None,
        verdict,
        upgrade_attempted: false,
    }
}

fn path_extension(path: &Path) -> Option<&str> {
    path.extension().and_then(|ext| ext.to_str())
}

fn measure_file(path: &Path) -> Result<Measured, ()> {
    let file = File::open(path).map_err(|_| ())?;
    let source = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path_extension(path) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|_| ())?;
    let mut format = probed.format;
    let track = format.default_track().ok_or(())?;
    let rate = track.codec_params.sample_rate.unwrap_or(0);
    if rate == 0 {
        return Err(());
    }
    let bit_depth = track.codec_params.bits_per_sample.unwrap_or(0);
    let frames = track.codec_params.n_frames.unwrap_or(0);
    let duration_secs =
        (frames > 0).then(|| u32::try_from(frames / u64::from(rate)).unwrap_or(u32::MAX));
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|_| ())?;
    let mut best: Option<f64> = None;
    if frames > u64::try_from(WINDOW).unwrap_or(u64::MAX) {
        for step in 1..=WINDOWS {
            let fraction = step as f64 / 10.0;
            let seconds = frames as f64 * fraction / f64::from(rate);
            let time = Time::new(seconds as u64, seconds.fract());
            if format
                .seek(
                    SeekMode::Accurate,
                    SeekTo::Time {
                        time,
                        track_id: Some(track_id),
                    },
                )
                .is_err()
            {
                continue;
            }
            decoder.reset();
            let block = pull_first(&mut format, &mut decoder, track_id, WINDOW);
            if let Some(hz) = block_cutoff(&block, rate) {
                best = Some(best.map_or(hz, |prev| prev.max(hz)));
            }
        }
    }
    if best.is_none() {
        decoder.reset();
        let block = pull_first(&mut format, &mut decoder, track_id, WINDOW);
        best = block_cutoff(&block, rate);
    }
    Ok(Measured {
        bit_depth,
        sample_rate: rate,
        duration_secs,
        cutoff_hz: best,
    })
}

fn pull_first(
    format: &mut Box<dyn symphonia::core::formats::FormatReader>,
    decoder: &mut Box<dyn symphonia::core::codecs::Decoder>,
    track_id: u32,
    max: usize,
) -> Vec<f32> {
    let mut out = Vec::with_capacity(max);
    let mut pending: Option<SampleBuffer<f32>> = None;
    while out.len() < max {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(AudioError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(_) => break,
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(AudioError::DecodeError(_)) => continue,
            Err(_) => break,
        };
        let spec = *decoded.spec();
        let channels = spec.channels.count().max(1);
        if pending.is_none() {
            pending = Some(SampleBuffer::<f32>::new(decoded.capacity() as u64, spec));
        }
        let Some(buffer) = pending.as_mut() else {
            continue;
        };
        buffer.copy_interleaved_ref(decoded);
        for frame in buffer.samples().chunks(channels) {
            if out.len() == max {
                break;
            }
            out.push(frame.first().copied().unwrap_or(0.0));
        }
    }
    out
}

fn block_cutoff(samples: &[f32], rate: u32) -> Option<f64> {
    if samples.len() != WINDOW || rate == 0 {
        return None;
    }
    let peak = samples
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    if peak < SILENCE_AMPLITUDE {
        return None;
    }
    let mut re = vec![0.0; WINDOW];
    let mut im = vec![0.0; WINDOW];
    for (index, sample) in samples.iter().enumerate() {
        re[index] = f64::from(*sample) * hann(index, WINDOW);
    }
    fft(&mut re, &mut im);
    let mut spectral = 0.0_f64;
    for bin in 1..WINDOW / 2 {
        spectral = spectral.max(magnitude(re[bin], im[bin]));
    }
    if spectral == 0.0 {
        return None;
    }
    let floor = spectral * FLOOR_RATIO;
    let mut last = 0usize;
    for bin in 1..WINDOW / 2 {
        if magnitude(re[bin], im[bin]) >= floor {
            last = bin;
        }
    }
    if last == 0 {
        return None;
    }
    Some(last as f64 * f64::from(rate) / WINDOW as f64)
}

fn hann(index: usize, len: usize) -> f64 {
    if len < 2 {
        return 1.0;
    }
    0.5 * (1.0 - (2.0 * std::f64::consts::PI * index as f64 / (len - 1) as f64).cos())
}

fn magnitude(re: f64, im: f64) -> f64 {
    re.hypot(im)
}

/// Radix-2 decimation in time. `re` and `im` have the same power-of-two length.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    if n < 2 || !n.is_power_of_two() || im.len() != n {
        return;
    }
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2usize;
    while len <= n {
        let angle = -2.0 * std::f64::consts::PI / len as f64;
        let step_re = angle.cos();
        let step_im = angle.sin();
        let mut start = 0usize;
        while start < n {
            let mut turn_re = 1.0;
            let mut turn_im = 0.0;
            for offset in 0..len / 2 {
                let even_re = re[start + offset];
                let even_im = im[start + offset];
                let odd_index = start + offset + len / 2;
                let odd_re = re[odd_index].mul_add(turn_re, -(im[odd_index] * turn_im));
                let odd_im = re[odd_index].mul_add(turn_im, im[odd_index] * turn_re);
                re[start + offset] = even_re + odd_re;
                im[start + offset] = even_im + odd_im;
                re[odd_index] = even_re - odd_re;
                im[odd_index] = even_im - odd_im;
                let next_re = turn_re.mul_add(step_re, -(turn_im * step_im));
                turn_im = turn_re.mul_add(step_im, turn_im * step_re);
                turn_re = next_re;
            }
            start += len;
        }
        len <<= 1;
    }
}

#[cfg(test)]
static PAUSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
static THREAD_NAME: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

#[cfg(test)]
fn note_thread() {
    if std::thread::current().name() != Some("quality") {
        return;
    }
    if let Ok(mut name) = THREAD_NAME.lock() {
        *name = "quality".to_owned();
    }
}

#[cfg(test)]
fn wait_if_paused() {
    if std::thread::current().name() != Some("quality") {
        return;
    }
    while PAUSED.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Holds quality workers before they decode. Tests use this to show the scan
/// returns while analysis is still waiting.
#[cfg(test)]
pub fn pause_workers(paused: bool) {
    PAUSED.store(paused, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
pub fn worker_thread_name() -> String {
    THREAD_NAME
        .lock()
        .map(|name| name.clone())
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) fn write_wav(path: &Path, rate: u32, bits: u16, samples: &[f32]) {
    let channels: u16 = 1;
    let bytes_per_sample = bits / 8;
    let block = channels * bytes_per_sample;
    let data_len = samples.len() * usize::from(block);
    let mut bytes = Vec::with_capacity(44 + data_len);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(
        &u32::try_from(36 + data_len)
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    let byte_rate = rate * u32::from(block);
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&block.to_le_bytes());
    bytes.extend_from_slice(&bits.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&u32::try_from(data_len).unwrap_or(u32::MAX).to_le_bytes());
    for sample in samples {
        let clipped = sample.clamp(-1.0, 1.0);
        if bits == 24 {
            let value = (f64::from(clipped) * f64::from(1_i32 << 23)).round() as i32;
            bytes.extend_from_slice(&value.to_le_bytes()[..3]);
        } else {
            let value = (f64::from(clipped) * f64::from(i16::MAX)).round() as i16;
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    std::fs::write(path, bytes).unwrap();
}

#[cfg(test)]
pub(crate) fn tone(rate: u32, freq: f32, count: usize) -> Vec<f32> {
    (0..count)
        .map(|index| {
            let t = index as f32 / rate as f32;
            (2.0 * std::f32::consts::PI * freq * t).sin() * 0.6
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(
        path: &str,
        bit_depth: Option<u32>,
        sample_rate: Option<u32>,
        duration: Option<u32>,
    ) -> SearchHit {
        SearchHit {
            user: "ada".to_owned(),
            path: path.to_owned(),
            size: 1_000,
            bitrate: None,
            duration,
            bit_depth,
            sample_rate,
            queue: 0,
            free_slot: true,
            upload_speed: 0,
            country: String::new(),
        }
    }

    #[test]
    fn a_known_sine_lands_in_its_bin() {
        let samples = tone(256, 8.0, 256);
        assert_eq!(dominant_bin(&samples), Some(8));
    }

    #[test]
    fn a_tone_near_20_khz_stays_under_the_lossless_line() {
        let samples = tone(44_100, 20_000.0, WINDOW);
        let cutoff = cutoff_hz(&samples, 44_100).unwrap();
        assert!(lossy_percent(cutoff, 44_100) <= LOSSLESS_MAX_PERCENT);
    }

    #[test]
    fn a_16_khz_wall_is_above_the_lossless_line() {
        let samples = tone(44_100, 16_000.0, WINDOW);
        let cutoff = cutoff_hz(&samples, 44_100).unwrap();
        assert!(lossy_percent(cutoff, 44_100) > LOSSLESS_MAX_PERCENT);
    }

    #[test]
    fn a_silent_block_does_not_set_the_cutoff() {
        assert!(cutoff_hz(&vec![0.0; WINDOW], 44_100).is_none());
    }

    #[test]
    fn a_full_band_wav_is_lossless_and_a_walled_wav_is_lossy() {
        let dir = std::env::temp_dir().join(format!(
            "soul-sever-quality-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let full = dir.join("full.wav");
        let walled = dir.join("walled.wav");
        let count = 44_100 * 2;
        write_wav(&full, 44_100, 16, &tone(44_100, 20_000.0, count));
        write_wav(&walled, 44_100, 16, &tone(44_100, 16_000.0, count));
        let full = analyze_path(&full);
        let walled = analyze_path(&walled);
        assert_eq!(full.verdict, Verdict::Lossless);
        assert!(full.lossy_percent.unwrap() <= LOSSLESS_MAX_PERCENT);
        assert_eq!(full.bit_depth, 16);
        assert_eq!(walled.verdict, Verdict::Lossy);
        assert!(walled.lossy_percent.unwrap() > LOSSLESS_MAX_PERCENT);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_undecodable_lossy_extension_is_lossy_without_a_percent() {
        let dir =
            std::env::temp_dir().join(format!("soul-sever-quality-wma-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("song.wma");
        std::fs::write(&path, b"not audio").unwrap();
        let quality = analyze_path(&path);
        assert_eq!(quality.verdict, Verdict::Lossy);
        assert!(quality.lossy_percent.is_none());
        let ape = dir.join("song.ape");
        std::fs::write(&ape, b"not audio").unwrap();
        assert_eq!(analyze_path(&ape).verdict, Verdict::Unmeasured);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn ranking_prefers_24_bit_and_then_the_higher_rate() {
        let hits = vec![
            hit(
                r"music\Ada\Album\01 Song.flac",
                Some(16),
                Some(44_100),
                Some(120),
            ),
            hit(
                r"music\Ada\Album\01 Song.flac",
                Some(24),
                Some(48_000),
                Some(120),
            ),
            hit(
                r"music\Ada\Album\01 Song.flac",
                Some(24),
                Some(96_000),
                Some(120),
            ),
            hit(r"other\live.mp3", Some(16), Some(44_100), Some(120)),
            hit(
                r"music\Ada\Album\01 Other.flac",
                Some(24),
                Some(192_000),
                Some(120),
            ),
            hit(
                r"music\Ada\Album\01 Song.flac",
                Some(24),
                Some(192_000),
                Some(200),
            ),
        ];
        let picked =
            pick_upgrade(&hits, UpgradeGoal::AnyLossless, "Ada", "Song", Some(120)).unwrap();
        assert_eq!(picked.bit_depth, Some(24));
        assert_eq!(picked.sample_rate, Some(96_000));
        let only_24 =
            pick_upgrade(&hits, UpgradeGoal::TwentyFour, "Ada", "Song", Some(120)).unwrap();
        assert!(only_24.bit_depth.unwrap() >= 24);
        assert_eq!(
            pick_upgrade(
                &[hit(
                    r"music\Ada\Album\01 Song.flac",
                    Some(16),
                    Some(192_000),
                    Some(120)
                )],
                UpgradeGoal::TwentyFour,
                "Ada",
                "Song",
                Some(120)
            ),
            None
        );
    }

    #[test]
    fn a_busy_user_yields_to_the_next_eligible_hit() {
        let mut best = hit(
            r"music\Ada\Album\01 Song.flac",
            Some(24),
            Some(96_000),
            Some(120),
        );
        best.user = "bob".to_owned();
        let mut other = hit(
            r"music\Ada\Album\01 Song.flac",
            Some(16),
            Some(44_100),
            Some(120),
        );
        other.user = "cara".to_owned();
        let hits = vec![best, other];
        let picked = pick_upgrade_except(
            &hits,
            UpgradeGoal::AnyLossless,
            "Ada",
            "Song",
            Some(120),
            &["bob"],
        )
        .unwrap();
        assert_eq!(picked.user, "cara");
        assert!(
            pick_upgrade_except(
                &hits,
                UpgradeGoal::AnyLossless,
                "Ada",
                "Song",
                Some(120),
                &["bob", "cara"]
            )
            .is_none()
        );
        assert!(pick_upgrade(&hits, UpgradeGoal::AnyLossless, "Ada", "Song", Some(120)).is_some());
    }

    #[test]
    fn a_different_artist_is_not_a_replacement() {
        let cover = hit(
            r"music\The Airborne Toxic Event\All I Need.flac",
            Some(24),
            Some(96_000),
            Some(120),
        );
        let same = hit(
            r"music\Air\Moon Safari\All I Need.flac",
            Some(16),
            Some(44_100),
            Some(120),
        );
        let songbird = hit(
            r"music\Air\Moon Safari\01 Songbird.flac",
            Some(24),
            Some(96_000),
            Some(120),
        );
        assert!(
            pick_upgrade(
                std::slice::from_ref(&cover),
                UpgradeGoal::AnyLossless,
                "Air",
                "All I Need",
                Some(120)
            )
            .is_none()
        );
        let hits = [cover, same];
        let picked = pick_upgrade(
            &hits,
            UpgradeGoal::AnyLossless,
            "Air",
            "All I Need",
            Some(120),
        )
        .unwrap();
        assert!(picked.path.contains("Moon Safari"));
        assert!(
            pick_upgrade(
                &[songbird],
                UpgradeGoal::AnyLossless,
                "Air",
                "Song",
                Some(120)
            )
            .is_none()
        );
    }

    #[test]
    fn a_16_bit_lossless_file_accepts_only_a_better_24_bit_file() {
        let cd = Quality {
            bit_depth: 16,
            sample_rate: 44_100,
            lossy_percent: Some(9),
            cutoff_hz: 20_000,
            duration_secs: Some(120),
            verdict: Verdict::Lossless,
            upgrade_attempted: false,
        };
        let hi = Quality {
            bit_depth: 24,
            sample_rate: 96_000,
            lossy_percent: Some(8),
            cutoff_hz: 44_000,
            duration_secs: Some(120),
            verdict: Verdict::Lossless,
            upgrade_attempted: false,
        };
        let another_cd = Quality {
            bit_depth: 16,
            ..cd.clone()
        };
        let upsampled = Quality {
            bit_depth: 24,
            sample_rate: 96_000,
            lossy_percent: Some(54),
            cutoff_hz: 22_000,
            duration_secs: Some(120),
            verdict: Verdict::Lossy,
            upgrade_attempted: false,
        };
        assert!(accepts_replacement(&cd, &hi));
        assert!(!accepts_replacement(&cd, &another_cd));
        assert!(!accepts_replacement(&cd, &upsampled));
        let lossy = Quality {
            verdict: Verdict::Lossy,
            lossy_percent: Some(27),
            ..cd.clone()
        };
        assert!(accepts_replacement(&lossy, &another_cd));
    }

    fn tag(path: &Path, artist: &str, album: &str, title: &str) {
        use lofty::config::WriteOptions;
        use lofty::file::{AudioFile, TaggedFileExt};
        use lofty::tag::{Accessor, Tag, TagType};
        let mut file = lofty::read_from_path(path).unwrap();
        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_artist(artist.to_owned());
        tag.set_album(album.to_owned());
        tag.set_title(title.to_owned());
        tag.set_track(1);
        file.insert_tag(tag);
        file.save_to_path(path, WriteOptions::default()).unwrap();
    }

    #[test]
    fn a_rejected_replacement_leaves_the_original_and_drops_the_stage() {
        let root =
            std::env::temp_dir().join(format!("soul-sever-replace-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let original_path = root.join("song.wav");
        let staged = root.join("stage.wav");
        let count = 44_100 * 2;
        write_wav(&original_path, 44_100, 16, &tone(44_100, 20_000.0, count));
        write_wav(&staged, 44_100, 16, &tone(44_100, 16_000.0, count));
        tag(&original_path, "Ada", "Album", "Song");
        tag(&staged, "Ada", "Album", "Song");
        let original = analyze_path(&original_path);
        assert_eq!(original.verdict, Verdict::Lossless);
        assert!(replace_with(&root, &original_path, &original, &staged).is_none());
        assert!(original_path.is_file());
        assert!(!staged.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_better_file_is_filed_before_the_original_is_removed() {
        let root =
            std::env::temp_dir().join(format!("soul-sever-replace-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let original_path = root.join("song.wav");
        let staged = root.join("stage.wav");
        let count = 44_100 * 2;
        write_wav(&original_path, 44_100, 16, &tone(44_100, 16_000.0, count));
        write_wav(&staged, 44_100, 16, &tone(44_100, 20_000.0, count));
        tag(&original_path, "Ada", "Album", "Song");
        tag(&staged, "Ada", "Album", "Song");
        let original = analyze_path(&original_path);
        assert_eq!(original.verdict, Verdict::Lossy);
        let filed = replace_with(&root, &original_path, &original, &staged).unwrap();
        assert!(!original_path.exists());
        assert!(filed.is_file());
        assert!(filed.starts_with(root.join("Ada").join("Album")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_replacement_keeps_the_original_artist_when_the_new_file_names_a_guest() {
        let root =
            std::env::temp_dir().join(format!("soul-sever-replace-guest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let original_path = root.join("song.wav");
        let staged = root.join("stage.wav");
        let count = 44_100 * 2;
        write_wav(&original_path, 44_100, 16, &tone(44_100, 16_000.0, count));
        write_wav(&staged, 44_100, 16, &tone(44_100, 20_000.0, count));
        tag(
            &original_path,
            "Nine Inch Nails",
            "The Fragile",
            "Somewhat Damaged",
        );
        tag(
            &staged,
            "Nine inch Nails and David Bowie",
            "Live",
            "Somewhat Damaged",
        );
        let original = analyze_path(&original_path);
        assert_eq!(original.verdict, Verdict::Lossy);
        let filed = replace_with(&root, &original_path, &original, &staged).unwrap();
        assert!(
            filed.starts_with(root.join("Nine Inch Nails").join("The Fragile")),
            "{}",
            filed.display()
        );
        assert!(!root.join("David Bowie").exists());
        assert!(!root.join("Nine inch Nails and David Bowie").exists());
        assert!(!root.join("Nine Inch Nails and David Bowie").exists());
        assert!(!original_path.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_lossless_16_bit_file_is_kept_until_a_24_bit_file_is_verified() {
        let root =
            std::env::temp_dir().join(format!("soul-sever-replace-24-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let original_path = root.join("song.wav");
        let same = root.join("same.wav");
        let count = 44_100 * 2;
        write_wav(&original_path, 44_100, 16, &tone(44_100, 20_000.0, count));
        write_wav(&same, 44_100, 16, &tone(44_100, 20_000.0, count));
        tag(&original_path, "Ada", "Album", "Song");
        tag(&same, "Ada", "Album", "Song");
        let original = analyze_path(&original_path);
        assert_eq!(original.verdict, Verdict::Lossless);
        assert_eq!(original.bit_depth, 16);
        assert!(replace_with(&root, &original_path, &original, &same).is_none());
        assert!(original_path.is_file());

        let better = root.join("better.wav");
        let hi = 96_000 * 2;
        write_wav(&better, 96_000, 24, &tone(96_000, 44_000.0, hi));
        tag(&better, "Ada", "Album", "Song");
        let filed = replace_with(&root, &original_path, &original, &better).unwrap();
        assert!(!original_path.exists());
        assert!(filed.is_file());
        assert_eq!(analyze_path(&filed).bit_depth, 24);
        let _ = std::fs::remove_dir_all(root);
    }
}
