//! Decode a song, play it on the default output device, and publish a spectrum.

use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::errors::Error as AudioError;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::library::Song;

const WINDOW: usize = 2048;
const BANDS: usize = 96;

struct Pcm {
    samples: Vec<f32>,
    rate: u32,
    cursor: f64,
    playing: bool,
    silent: bool,
    artist: String,
    album: String,
    title: String,
    path: std::path::PathBuf,
    kind: String,
    bytes: u64,
    bit_depth: u32,
    channels: u32,
    error: String,
    /// Full length in mono samples, from the file when the container knows it.
    total: usize,
    /// The decoder has read the last packet.
    done: bool,
    /// Bands last drawn. Empty until the first frame of this song.
    shown: Vec<f32>,
    /// Recent peak, so a quieter frame can fall instead of filling the meter.
    peak: f32,
}

impl Default for Pcm {
    fn default() -> Self {
        Self {
            samples: Vec::new(),
            rate: 44_100,
            cursor: 0.0,
            playing: false,
            silent: true,
            artist: String::new(),
            album: String::new(),
            title: String::new(),
            path: std::path::PathBuf::new(),
            kind: String::new(),
            bytes: 0,
            bit_depth: 0,
            channels: 0,
            error: String::new(),
            total: 0,
            done: false,
            shown: Vec::new(),
            peak: 0.0,
        }
    }
}

/// One song at a time. The spectrum follows the samples leaving the speakers.
pub struct Player {
    shared: Arc<Mutex<Pcm>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Mutex::new(Pcm::default())),
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
        }
    }

    pub fn play(&mut self, song: &Song) {
        self.halt();
        {
            let mut pcm = lock(&self.shared);
            *pcm = Pcm {
                artist: song.artist.clone(),
                album: song.album.clone(),
                title: song.title.clone(),
                path: song.path.clone(),
                kind: file_kind(&song.path),
                bytes: file_size(&song.path),
                ..Pcm::default()
            };
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = Arc::clone(&stop);
        let shared = Arc::clone(&self.shared);
        let path = song.path.clone();
        self.thread = std::thread::Builder::new()
            .name("playback".to_owned())
            .spawn(move || run(path, shared, stop))
            .ok();
    }

    pub fn toggle(&self) {
        let mut pcm = lock(&self.shared);
        if pcm.samples.is_empty() {
            return;
        }
        if ended(&pcm) {
            pcm.cursor = 0.0;
            pcm.playing = true;
            return;
        }
        pcm.playing = !pcm.playing;
    }

    /// Starts or resumes the loaded song. A song already at the end starts over.
    pub fn resume(&self) {
        let mut pcm = lock(&self.shared);
        if pcm.samples.is_empty() {
            return;
        }
        if ended(&pcm) {
            pcm.cursor = 0.0;
        }
        pcm.playing = true;
    }

    pub fn pause(&self) {
        lock(&self.shared).playing = false;
    }

    /// Returns to the first sample and leaves the song paused.
    pub fn stop(&self) {
        let mut pcm = lock(&self.shared);
        pcm.cursor = 0.0;
        pcm.playing = false;
        pcm.shown.clear();
        pcm.peak = 0.0;
    }

    /// Stops and drops the decoder when the loaded file is one of `paths`.
    pub fn drop_if(&mut self, paths: &[std::path::PathBuf]) {
        let current = lock(&self.shared).path.clone();
        if current.as_os_str().is_empty() || !paths.iter().any(|path| path == &current) {
            return;
        }
        self.halt();
        *lock(&self.shared) = Pcm::default();
    }

    pub fn loaded_path(&self) -> std::path::PathBuf {
        lock(&self.shared).path.clone()
    }

    pub fn is_playing(&self) -> bool {
        let pcm = lock(&self.shared);
        pcm.playing && !pcm.samples.is_empty()
    }

    /// Places the playhead at `fraction` of the decoded song. `0.0` is the start and `1.0` is the last sample.
    pub fn seek(&self, fraction: f64) {
        let mut pcm = lock(&self.shared);
        if pcm.samples.is_empty() {
            return;
        }
        pcm.cursor = sample_cursor(fraction, song_len(&pcm));
        pcm.shown.clear();
        pcm.peak = 0.0;
    }

    /// Moves a song forward when no output device is open.
    pub fn advance(&self, elapsed: Duration) {
        let mut pcm = lock(&self.shared);
        if !pcm.silent || !pcm.playing || pcm.samples.is_empty() {
            return;
        }
        pcm.cursor += cursor_step(pcm.rate, elapsed);
        if ended(&pcm) {
            pcm.cursor = (song_len(&pcm) as f64 - 1.0).max(0.0);
            pcm.playing = false;
        }
    }

    pub fn spectrum(&self) -> Vec<f32> {
        let mut pcm = lock(&self.shared);
        if pcm.samples.is_empty() {
            return vec![0.0; BANDS];
        }
        if !pcm.playing {
            if pcm.shown.is_empty() {
                let frame = window_bands(&mut pcm, false);
                pcm.shown = frame;
            }
            return pcm.shown.clone();
        }
        let target = window_bands(&mut pcm, true);
        pcm.shown = ease_bands(&pcm.shown, &target);
        pcm.shown.clone()
    }

    pub fn headline(&self) -> String {
        let pcm = lock(&self.shared);
        if !pcm.error.is_empty() {
            return pcm.error.clone();
        }
        if pcm.title.is_empty() {
            return "nothing playing".to_owned();
        }
        let state = if pcm.samples.is_empty() {
            "opening"
        } else {
            state_word(
                pcm.playing,
                pcm.silent,
                pcm.cursor < f64::from(pcm.rate.max(1)) * 0.05,
                ended(&pcm),
            )
        };
        format!("{} — {}   {}   {state}", pcm.artist, pcm.title, pcm.album)
    }

    pub fn ready(&self) -> bool {
        let pcm = lock(&self.shared);
        !pcm.samples.is_empty() || !pcm.error.is_empty()
    }

    pub fn loaded(&self) -> bool {
        !lock(&self.shared).samples.is_empty()
    }

    pub(crate) fn snapshot(&self) -> Snapshot {
        let pcm = lock(&self.shared);
        let duration = duration_of(song_len(&pcm), pcm.rate);
        let position = duration_of_cursor(pcm.cursor, pcm.rate);
        let bitrate = bitrate_kbps(
            pcm.bit_depth,
            pcm.rate,
            pcm.channels,
            pcm.bytes,
            duration.as_secs_f64(),
        );
        Snapshot {
            artist: pcm.artist.clone(),
            album: pcm.album.clone(),
            title: pcm.title.clone(),
            kind: pcm.kind.clone(),
            bytes: pcm.bytes,
            bitrate_kbps: bitrate,
            sample_rate: pcm.rate,
            bit_depth: pcm.bit_depth,
            channels: pcm.channels,
            position,
            duration,
            playing: pcm.playing && !pcm.samples.is_empty(),
            silent: pcm.silent,
            loaded: !pcm.samples.is_empty(),
            error: pcm.error.clone(),
        }
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.halt();
    }
}

fn run(path: std::path::PathBuf, shared: Arc<Mutex<Pcm>>, stop: Arc<AtomicBool>) {
    let mut source = match SongSource::open(&path) {
        Ok(source) => source,
        Err(err) => {
            lock(&shared).error = err;
            return;
        }
    };
    let first = source.pull(preroll_samples(source.rate), &stop);
    if stop.load(Ordering::Relaxed) {
        return;
    }
    if first.is_empty() {
        lock(&shared).error = "no samples in the file".to_owned();
        return;
    }
    {
        let mut pcm = lock(&shared);
        pcm.rate = source.rate;
        pcm.bit_depth = source.bit_depth;
        pcm.channels = source.channels;
        pcm.total = source.total;
        pcm.samples = first;
        pcm.done = source.done;
        pcm.cursor = 0.0;
        pcm.playing = true;
        pcm.silent = false;
        pcm.error.clear();
    }
    if stop.load(Ordering::Relaxed) {
        return;
    }
    // Dropping the stream stops the device. It has to stay here until playback stops,
    // or the screen says "playing" while the speakers stay silent.
    let output = match open_output(Arc::clone(&shared)) {
        Ok(stream) => match stream.play() {
            Ok(()) => Some(stream),
            Err(_) => {
                lock(&shared).silent = true;
                None
            }
        },
        Err(_) => {
            lock(&shared).silent = true;
            None
        }
    };
    while !source.done && !stop.load(Ordering::Relaxed) {
        let more = source.pull(source.rate.max(1) as usize, &stop);
        if more.is_empty() {
            break;
        }
        let mut pcm = lock(&shared);
        pcm.samples.extend(more);
        pcm.channels = source.channels;
        pcm.done = source.done;
        if source.total > pcm.total {
            pcm.total = source.total;
        }
    }
    if !stop.load(Ordering::Relaxed) {
        let mut pcm = lock(&shared);
        pcm.done = true;
        if pcm.samples.len() > pcm.total {
            pcm.total = pcm.samples.len();
        }
    }
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(40));
    }
    drop(output);
}

fn open_output(shared: Arc<Mutex<Pcm>>) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "no output device".to_owned())?;
    let configs = device
        .supported_output_configs()
        .map_err(|err| err.to_string())?;
    let range = configs
        .into_iter()
        .find(|range| range.sample_format() == cpal::SampleFormat::F32)
        .ok_or_else(|| "output device has no f32 stream".to_owned())?;
    let config = range.with_max_sample_rate().config();
    let channels = usize::from(config.channels.max(1));
    let device_rate = config.sample_rate.0.max(1);
    device
        .build_output_stream(
            &config,
            move |data: &mut [f32], _| render(&shared, data, channels, device_rate),
            |_| {},
            None,
        )
        .map_err(|err| err.to_string())
}

fn render(shared: &Mutex<Pcm>, data: &mut [f32], channels: usize, device_rate: u32) {
    let mut pcm = lock(shared);
    if !pcm.playing || pcm.samples.is_empty() {
        data.fill(0.0);
        return;
    }
    let ratio = f64::from(pcm.rate.max(1)) / f64::from(device_rate.max(1));
    let channels = channels.max(1);
    for frame in data.chunks_mut(channels) {
        if ended(&pcm) {
            pcm.playing = false;
            frame.fill(0.0);
            continue;
        }
        if pcm.cursor >= pcm.samples.len() as f64 {
            frame.fill(0.0);
            continue;
        }
        let sample = sample_at(&pcm.samples, pcm.cursor);
        pcm.cursor += ratio;
        for slot in frame {
            *slot = sample;
        }
    }
}

fn song_len(pcm: &Pcm) -> usize {
    if pcm.total > 0 {
        pcm.total
    } else {
        pcm.samples.len()
    }
}

fn ended(pcm: &Pcm) -> bool {
    let len = song_len(pcm);
    len > 0 && pcm.cursor + 1.0 >= len as f64 && (pcm.done || pcm.total > 0)
}

/// How much audio has to be decoded before the device starts. The rest follows while it plays.
fn preroll_samples(rate: u32) -> usize {
    let slice = usize::try_from(rate.max(1) / 5).unwrap_or(WINDOW);
    slice.max(WINDOW)
}

fn sample_at(samples: &[f32], cursor: f64) -> f32 {
    let index = cursor.floor().max(0.0) as usize;
    let next = index.saturating_add(1).min(samples.len().saturating_sub(1));
    let left = samples.get(index).copied().unwrap_or(0.0);
    let right = samples.get(next).copied().unwrap_or(left);
    let mix = (cursor - index as f64).clamp(0.0, 1.0) as f32;
    left + (right - left) * mix
}

struct SongSource {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    rate: u32,
    bit_depth: u32,
    channels: u32,
    total: usize,
    pending: Vec<f32>,
    buffer: Option<SampleBuffer<f32>>,
    done: bool,
    packets: usize,
}

impl SongSource {
    fn open(path: &Path) -> Result<Self, String> {
        let file =
            File::open(path).map_err(|err| format!("could not open {}: {err}", path.display()))?;
        let source = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|ext| ext.to_str()) {
            hint.with_extension(ext);
        }
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                source,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .map_err(|err| err.to_string())?;
        let format = probed.format;
        let track = format
            .default_track()
            .ok_or_else(|| "no audio track".to_owned())?;
        let rate = track.codec_params.sample_rate.unwrap_or(44_100).max(1);
        let bit_depth = track.codec_params.bits_per_sample.unwrap_or(0);
        let total = track
            .codec_params
            .n_frames
            .and_then(|frames| usize::try_from(frames).ok())
            .unwrap_or(0);
        let track_id = track.id;
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|err| err.to_string())?;
        Ok(Self {
            format,
            decoder,
            track_id,
            rate,
            bit_depth,
            channels: 0,
            total,
            pending: Vec::new(),
            buffer: None,
            done: false,
            packets: 0,
        })
    }

    /// Decodes at most `max` mono samples. One call does not read the rest of the file.
    fn pull(&mut self, max: usize, stop: &AtomicBool) -> Vec<f32> {
        let mut out = Vec::new();
        while out.len() < max && !self.done && !stop.load(Ordering::Relaxed) {
            if self.pending.is_empty() && !self.fill_pending() {
                self.done = true;
                break;
            }
            let take = (max - out.len()).min(self.pending.len());
            out.extend(self.pending.drain(..take));
        }
        out
    }

    fn fill_pending(&mut self) -> bool {
        loop {
            let packet = match self.format.next_packet() {
                Ok(packet) => packet,
                Err(AudioError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(_) => return false,
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            self.packets += 1;
            let decoded = match self.decoder.decode(&packet) {
                Ok(decoded) => decoded,
                Err(AudioError::DecodeError(_)) => continue,
                Err(_) => return false,
            };
            let spec = *decoded.spec();
            let channel_count = spec.channels.count().max(1);
            self.channels = u32::try_from(channel_count).unwrap_or(u32::MAX);
            if self.buffer.is_none() {
                self.buffer = Some(SampleBuffer::<f32>::new(decoded.capacity() as u64, spec));
            }
            let Some(samples) = self.buffer.as_mut() else {
                continue;
            };
            samples.copy_interleaved_ref(decoded);
            for frame in samples.samples().chunks(channel_count) {
                let sum: f32 = frame.iter().copied().sum();
                self.pending.push(sum / channel_count as f32);
            }
            return true;
        }
    }
}

/// Where a column sits on a seek bar. Adjacent columns stay on different fractions.
pub fn bar_fraction(column: u16, origin: u16, width: u16) -> f64 {
    if width <= 1 {
        return 0.0;
    }
    let offset = column.saturating_sub(origin).min(width - 1);
    f64::from(offset) / f64::from(width - 1)
}

pub fn sample_cursor(fraction: f64, len: usize) -> f64 {
    if len == 0 {
        return 0.0;
    }
    fraction.clamp(0.0, 1.0) * (len - 1) as f64
}

fn cursor_step(rate: u32, elapsed: Duration) -> f64 {
    f64::from(rate.max(1)) * elapsed.as_secs_f64()
}

/// Fast rise, slower fall. The first frame copies the target so a new song is not delayed.
fn ease_bands(shown: &[f32], target: &[f32]) -> Vec<f32> {
    const ATTACK: f32 = 0.85;
    const RELEASE: f32 = 0.35;
    if shown.len() != target.len() {
        return target.to_vec();
    }
    shown
        .iter()
        .zip(target)
        .map(|(shown, target)| {
            let rate = if *target >= *shown { ATTACK } else { RELEASE };
            shown + (target - shown) * rate
        })
        .collect()
}

fn hold_peak(peak: f32, frame_max: f32) -> f32 {
    (peak * 0.92).max(frame_max)
}

fn scale_bands(raw: &[f32], peak: f32) -> Vec<f32> {
    if peak <= 0.0 {
        return vec![0.0; raw.len()];
    }
    raw.iter()
        .map(|value| (value / peak).clamp(0.0, 1.0))
        .collect()
}

fn window_bands(pcm: &mut Pcm, follow: bool) -> Vec<f32> {
    let end = (pcm.cursor.floor() as usize)
        .saturating_add(WINDOW)
        .min(pcm.samples.len());
    let start = end.saturating_sub(WINDOW);
    let raw = band_peaks(&pcm.samples[start..end], pcm.rate.max(1), BANDS);
    let frame_max = raw.iter().copied().fold(0.0f32, f32::max);
    pcm.peak = if follow {
        hold_peak(pcm.peak, frame_max)
    } else {
        frame_max
    };
    scale_bands(&raw, pcm.peak)
}

fn file_kind(path: &Path) -> String {
    path.extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("audio")
        .to_ascii_uppercase()
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

fn duration_of(samples: usize, rate: u32) -> Duration {
    if samples == 0 || rate == 0 {
        return Duration::ZERO;
    }
    Duration::from_secs_f64(samples as f64 / f64::from(rate))
}

fn duration_of_cursor(cursor: f64, rate: u32) -> Duration {
    if rate == 0 {
        return Duration::ZERO;
    }
    Duration::from_secs_f64((cursor.max(0.0)) / f64::from(rate))
}

fn bitrate_kbps(bit_depth: u32, rate: u32, channels: u32, bytes: u64, seconds: f64) -> u32 {
    if bit_depth > 0 && channels > 0 && rate > 0 {
        return rate.saturating_mul(bit_depth).saturating_mul(channels) / 1000;
    }
    if seconds > 0.0 && bytes > 0 {
        return ((bytes as f64) * 8.0 / seconds / 1000.0).round() as u32;
    }
    0
}

/// What the playback screen shows beside the meter and on the transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub artist: String,
    pub album: String,
    pub title: String,
    pub kind: String,
    pub bytes: u64,
    pub bitrate_kbps: u32,
    pub sample_rate: u32,
    pub bit_depth: u32,
    pub channels: u32,
    pub position: Duration,
    pub duration: Duration,
    pub playing: bool,
    pub silent: bool,
    pub loaded: bool,
    pub error: String,
}

impl Snapshot {
    pub fn rows(&self) -> Vec<String> {
        if !self.error.is_empty() {
            return vec![self.error.clone()];
        }
        if self.title.is_empty() {
            return vec!["nothing playing".to_owned()];
        }
        let mut rows = vec![self.title.clone(), self.artist.clone(), self.album.clone()];
        if !self.loaded {
            if !self.kind.is_empty() {
                rows.push(self.kind.clone());
            }
            rows.push("opening".to_owned());
            return rows;
        }
        let mut kind = self.kind.clone();
        if self.bitrate_kbps > 0 {
            kind.push_str(&format!(" · {} kbps", self.bitrate_kbps));
        }
        rows.push(kind);
        let mut signal = format_hz(self.sample_rate);
        if self.bit_depth > 0 {
            signal.push_str(&format!(" · {}-bit", self.bit_depth));
        }
        if self.channels > 0 {
            signal.push_str(" · ");
            signal.push_str(channel_name(self.channels));
        }
        rows.push(signal);
        rows.push(format!(
            "{} · {}",
            format_clock(self.duration),
            format_size(self.bytes)
        ));
        rows.push(self.status().to_owned());
        rows
    }

    pub fn position_text(&self) -> String {
        format_clock(self.position)
    }

    pub fn length_text(&self) -> String {
        format_clock(self.duration)
    }

    pub fn fraction(&self) -> f64 {
        let total = self.duration.as_secs_f64();
        if total <= 0.0 {
            0.0
        } else {
            (self.position.as_secs_f64() / total).clamp(0.0, 1.0)
        }
    }

    fn status(&self) -> &'static str {
        state_word(
            self.playing,
            self.silent,
            self.loaded && self.position < Duration::from_millis(50),
            self.loaded && self.position + Duration::from_millis(20) >= self.duration,
        )
    }
}

fn state_word(playing: bool, silent: bool, at_start: bool, at_end: bool) -> &'static str {
    if playing && silent {
        "playing, no output device"
    } else if playing {
        "playing"
    } else if at_start || at_end {
        "stopped"
    } else {
        "paused"
    }
}

pub fn format_clock(duration: Duration) -> String {
    let millis = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
    let minutes = millis / 60_000;
    let seconds = (millis / 1_000) % 60;
    let hundredths = (millis % 1_000) / 10;
    format!("{minutes}:{seconds:02}.{hundredths:02}")
}

fn format_hz(rate: u32) -> String {
    if rate >= 1_000 && rate.is_multiple_of(1_000) {
        format!("{} kHz", rate / 1_000)
    } else if rate >= 1_000 {
        format!("{:.1} kHz", rate as f64 / 1_000.0)
    } else if rate == 0 {
        "—".to_owned()
    } else {
        format!("{rate} Hz")
    }
}

fn format_size(bytes: u64) -> String {
    if bytes < 1_024 {
        return format!("{bytes} B");
    }
    let kb = bytes as f64 / 1_024.0;
    if kb < 1_024.0 {
        format!("{kb:.1} KB")
    } else {
        format!("{:.1} MB", kb / 1_024.0)
    }
}

fn channel_name(channels: u32) -> &'static str {
    match channels {
        1 => "mono",
        2 => "stereo",
        _ => "multi",
    }
}

fn lock(shared: &Mutex<Pcm>) -> MutexGuard<'_, Pcm> {
    shared.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Log-spaced magnitudes from about 40 Hz up to the Nyquist frequency, each 0 to 1.
#[cfg(test)]
fn frequency_bands(samples: &[f32], rate: u32, count: usize) -> Vec<f32> {
    let mut bands = band_peaks(samples, rate, count);
    let max = bands.iter().copied().fold(0.0f32, f32::max);
    if max > 0.0 {
        for value in &mut bands {
            *value = (*value / max).clamp(0.0, 1.0);
        }
    }
    bands
}

fn band_peaks(samples: &[f32], rate: u32, count: usize) -> Vec<f32> {
    if count == 0 {
        return Vec::new();
    }
    let magnitudes = magnitudes(samples);
    let nyquist = rate.max(1) as f32 / 2.0;
    let low = 40.0f32.min(nyquist * 0.5).max(1.0);
    let high = nyquist.max(low * 1.01);
    let bins = magnitudes.len();
    let mut bands = vec![0.0f32; count];
    for (band, value) in bands.iter_mut().enumerate() {
        let start_hz = low * (high / low).powf(band as f32 / count as f32);
        let end_hz = low * (high / low).powf((band + 1) as f32 / count as f32);
        let first = hz_to_bin(start_hz, rate, WINDOW).min(bins.saturating_sub(1));
        let last = hz_to_bin(end_hz, rate, WINDOW)
            .max(first.saturating_add(1))
            .min(bins);
        *value = magnitudes[first..last]
            .iter()
            .copied()
            .fold(0.0f32, f32::max);
    }
    bands
}

fn hz_to_bin(hz: f32, rate: u32, window: usize) -> usize {
    let bin = hz / rate.max(1) as f32 * window as f32;
    bin.round().max(0.0) as usize
}

fn magnitudes(samples: &[f32]) -> Vec<f32> {
    let mut real = [0.0f32; WINDOW];
    let mut imag = [0.0f32; WINDOW];
    let count = samples.len().min(WINDOW);
    for (index, sample) in samples.iter().copied().take(count).enumerate() {
        let window = 0.5 - 0.5 * (std::f32::consts::TAU * index as f32 / WINDOW as f32).cos();
        real[index] = sample * window;
    }
    let mut reversed = 0;
    for index in 1..WINDOW {
        let mut bit = WINDOW >> 1;
        while reversed & bit != 0 {
            reversed ^= bit;
            bit >>= 1;
        }
        reversed ^= bit;
        if index < reversed {
            real.swap(index, reversed);
            imag.swap(index, reversed);
        }
    }
    let mut length = 2;
    while length <= WINDOW {
        let angle = -std::f32::consts::TAU / length as f32;
        let turn_real = angle.cos();
        let turn_imag = angle.sin();
        let mut offset = 0;
        while offset < WINDOW {
            let mut turn = (1.0f32, 0.0f32);
            for step in 0..length / 2 {
                let even = offset + step;
                let odd = even + length / 2;
                let mixed_real = real[odd] * turn.0 - imag[odd] * turn.1;
                let mixed_imag = real[odd] * turn.1 + imag[odd] * turn.0;
                real[odd] = real[even] - mixed_real;
                imag[odd] = imag[even] - mixed_imag;
                real[even] += mixed_real;
                imag[even] += mixed_imag;
                let next_real = turn.0 * turn_real - turn.1 * turn_imag;
                turn.1 = turn.0 * turn_imag + turn.1 * turn_real;
                turn.0 = next_real;
            }
            offset += length;
        }
        length <<= 1;
    }
    (0..WINDOW / 2)
        .map(|index| (real[index] * real[index] + imag[index] * imag[index]).sqrt())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_low_tone_peaks_left_of_a_high_tone() {
        let low = tone(44_100, 440.0);
        let high = tone(44_100, 8_000.0);
        let low_bands = frequency_bands(&low, 44_100, BANDS);
        let high_bands = frequency_bands(&high, 44_100, BANDS);
        let low_peak = peak_at(&low_bands);
        let high_peak = peak_at(&high_bands);
        assert!(low_peak < 48, "{low_peak}");
        assert!(high_peak > 60, "{high_peak}");
        assert!(low_peak < high_peak);
        assert!(low_bands[low_peak] > 0.9);
        assert!(high_bands[high_peak] > 0.9);
    }

    #[test]
    fn adjacent_seek_columns_land_on_different_samples() {
        assert_eq!(bar_fraction(0, 10, 101), 0.0);
        assert_eq!(bar_fraction(110, 10, 101), 1.0);
        let left = bar_fraction(60, 10, 101);
        let right = bar_fraction(61, 10, 101);
        assert!((left - 0.5).abs() < f64::EPSILON);
        assert!(right - left > 0.009);
        let samples = 8_001;
        assert_eq!(sample_cursor(left, samples), 4_000.0);
        assert!(sample_cursor(right, samples) - sample_cursor(left, samples) >= 80.0);
        assert_eq!(sample_cursor(1.0, samples), 8_000.0);
        assert_eq!(sample_cursor(0.5, 0), 0.0);
    }

    #[test]
    fn a_playback_frame_stays_inside_the_spectrum_window() {
        let frame = cursor_step(44_100, Duration::from_millis(33));
        assert!(frame > 0.0);
        assert!(frame < WINDOW as f64, "{frame}");
        assert!(cursor_step(44_100, Duration::from_millis(200)) > WINDOW as f64);
    }

    #[test]
    fn the_meter_rises_faster_than_it_falls() {
        let risen = ease_bands(&[0.0], &[1.0]);
        assert!(risen[0] > 0.8 && risen[0] < 1.0, "{}", risen[0]);
        let fallen = ease_bands(&risen, &[0.0]);
        assert!(fallen[0] > 0.0);
        assert!(risen[0] - fallen[0] < risen[0]);
    }

    #[test]
    fn playback_can_start_before_the_rest_of_the_file_is_decoded() {
        let path = std::env::temp_dir().join(format!(
            "soul-sever-preroll-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        write_tone(&path, 8_000, 8_000);
        let mut source = SongSource::open(&path).unwrap();
        let stop = AtomicBool::new(false);
        let first = source.pull(preroll_samples(source.rate), &stop);
        assert_eq!(first.len(), preroll_samples(8_000));
        assert!(first.len() < 8_000);
        assert!(!source.done);
        assert_eq!(source.total, 8_000);
        let packets = source.packets;
        let mut rest = Vec::new();
        while !source.done {
            let more = source.pull(65_536, &stop);
            if more.is_empty() {
                break;
            }
            rest.extend(more);
        }
        assert!(source.packets > packets);
        assert_eq!(first.len() + rest.len(), 8_000);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn an_open_device_moves_the_playhead() {
        let path = std::env::temp_dir().join(format!(
            "soul-sever-live-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        write_tone(&path, 8_000, 16_000);
        let song = Song {
            artist: "Hail the Sun".to_owned(),
            album: "Wake".to_owned(),
            title: "Live".to_owned(),
            track: 1,
            path: path.clone(),
        };
        let mut player = Player::new();
        player.play(&song);
        let started = std::time::Instant::now();
        while !player.ready() && started.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let opened = player.snapshot();
        assert!(opened.playing, "{}", opened.error);
        assert!(!opened.silent, "this machine has no output device");
        let mut heard = false;
        while started.elapsed() < Duration::from_secs(2) {
            if player.snapshot().position > Duration::from_millis(50) {
                heard = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(player);
        let _ = std::fs::remove_file(path);
        assert!(
            heard,
            "the screen said playing, but the playhead never left the start"
        );
    }

    #[test]
    fn a_loud_frame_keeps_the_next_quiet_frame_below_full_scale() {
        let peak = hold_peak(hold_peak(0.0, 1.0), 0.2);
        assert!(peak > 0.2);
        let shown = scale_bands(&[0.2], peak);
        assert!(shown[0] < 0.5, "{}", shown[0]);
        assert!((hold_peak(0.2, 0.9) - 0.9).abs() < f32::EPSILON);
    }

    fn peak_at(bands: &[f32]) -> usize {
        bands
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.total_cmp(right.1))
            .map(|(index, _)| index)
            .unwrap_or(0)
    }

    fn tone(rate: u32, freq: f32) -> Vec<f32> {
        (0..WINDOW)
            .map(|index| {
                let step = index as f32 / rate as f32;
                (step * freq * std::f32::consts::TAU).sin()
            })
            .collect()
    }

    fn write_tone(path: &Path, rate: u32, frames: u32) {
        let mut bytes = Vec::new();
        let data_len = frames.saturating_mul(2);
        bytes.extend(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for index in 0..frames {
            let step = index as f32 / rate as f32;
            let sample = (step * 440.0 * std::f32::consts::TAU).sin();
            bytes.extend_from_slice(&((sample * 12_000.0) as i16).to_le_bytes());
        }
        std::fs::write(path, bytes).unwrap();
    }
}
