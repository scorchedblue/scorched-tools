//! Peak audio metering for the `audio-levels` subcommand.
//!
//! Ported from `scorched-desktop/quickshell/audio-levels.sh`. That script
//! piped `parec` through `od` and `awk` to turn raw PCM into a peak level;
//! every tick spawned two extra processes to do arithmetic a few lines of
//! Rust can do directly on the bytes `parec` already produces.
//!
//! 8 kHz mono is far below anything worth listening to, but it is plenty for
//! a meter, and it keeps the sample rate cheap. A batch of 384 samples is
//! ~48ms at that rate, or about 20 updates a second -- faster than that is
//! below what the eye resolves on a bar and only wakes the reader more often.

use std::io::{self, Read, Write};
use std::process::{Command, ExitCode, Stdio};

/// Samples per emitted level: 384, ~48ms at 8 kHz, ~20 updates a second.
pub const BATCH_SIZE: usize = 384;

/// Turns a stream of raw `s16le` mono samples into one peak level, 0-100, per
/// `batch_size` samples.
///
/// Bytes are fed in via [`Self::push`] as they arrive, so a sample split
/// across two reads is carried over rather than dropped.
#[derive(Debug)]
pub struct LevelMeter {
    batch_size: usize,
    peak: u32,
    count: usize,
    pending_byte: Option<u8>,
}

impl LevelMeter {
    #[must_use]
    pub fn new(batch_size: usize) -> Self {
        Self {
            batch_size,
            peak: 0,
            count: 0,
            pending_byte: None,
        }
    }

    /// Feeds in more raw bytes, returning one level, 0-100, for each batch
    /// completed by this call. A trailing odd byte or partial batch is held
    /// over for the next call rather than emitted or discarded.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<u32> {
        let mut levels = Vec::new();
        let mut chunk = bytes;

        if let Some(lo) = self.pending_byte.take() {
            if let Some(&hi) = chunk.first() {
                self.sample(i16::from_le_bytes([lo, hi]));
                chunk = &chunk[1..];
                if let Some(level) = self.maybe_emit() {
                    levels.push(level);
                }
            } else {
                self.pending_byte = Some(lo);
                return levels;
            }
        }

        let (pairs, remainder) = chunk.as_chunks::<2>();
        for &[lo, hi] in pairs {
            self.sample(i16::from_le_bytes([lo, hi]));
            if let Some(level) = self.maybe_emit() {
                levels.push(level);
            }
        }
        if let [lo] = *remainder {
            self.pending_byte = Some(lo);
        }

        levels
    }

    fn sample(&mut self, sample: i16) {
        let magnitude = i32::from(sample).unsigned_abs();
        self.peak = self.peak.max(magnitude);
        self.count += 1;
    }

    fn maybe_emit(&mut self) -> Option<u32> {
        if self.count < self.batch_size {
            return None;
        }
        let level = (self.peak * 100) / 32768;
        self.peak = 0;
        self.count = 0;
        Some(level)
    }
}

/// Reads raw `s16le` mono PCM from `reader` and writes one peak level per
/// line to `writer`, flushed immediately so a reader sees it as it happens.
fn meter_stream<R: Read, W: Write>(mut reader: R, mut writer: W) -> io::Result<()> {
    let mut meter = LevelMeter::new(BATCH_SIZE);
    let mut buf = [0u8; 4096];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        for level in meter.push(&buf[..n]) {
            writeln!(writer, "{level}")?;
            writer.flush()?;
        }
    }
}

/// Runs the `audio-levels` subcommand: spawns `parec` against `source` and
/// meters its output until the process ends or errors.
///
/// `source` names a PulseAudio/PipeWire source -- a microphone to meter it
/// directly, or a sink's `.monitor` source to meter what it is playing.
#[must_use]
pub fn run(source: Option<&str>) -> ExitCode {
    let Some(source) = source.filter(|s| !s.is_empty()) else {
        eprintln!("usage: scorched audio-levels <pulse-source-name>");
        return ExitCode::from(2);
    };

    // --latency-msec and --process-time-msec keep parec's buffering low
    // enough that the meter does not visibly trail the sound. --raw
    // suppresses the WAV header, which would otherwise read as samples and
    // show up as one loud spike at startup.
    let mut child = match Command::new("parec")
        .arg(format!("--device={source}"))
        .args(["--format=s16le", "--rate=8000", "--channels=1", "--raw"])
        .args(["--latency-msec=20", "--process-time-msec=10"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            eprintln!("scorched: failed to start parec: {err}");
            return ExitCode::FAILURE;
        }
    };

    let Some(stdout) = child.stdout.take() else {
        eprintln!("scorched: parec started without a stdout pipe");
        return ExitCode::FAILURE;
    };

    let result = meter_stream(stdout, io::stdout());
    let _ = child.wait();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("scorched: audio-levels: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LevelMeter;

    fn samples_to_bytes(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn a_full_batch_of_silence_emits_zero() {
        let mut meter = LevelMeter::new(4);
        let bytes = samples_to_bytes(&[0, 0, 0, 0]);
        assert_eq!(meter.push(&bytes), vec![0]);
    }

    #[test]
    fn peak_is_the_loudest_sample_in_the_batch() {
        let mut meter = LevelMeter::new(4);
        let bytes = samples_to_bytes(&[10, -20000, 100, -5]);
        assert_eq!(meter.push(&bytes), vec![(20000 * 100) / 32768]);
    }

    #[test]
    fn full_scale_negative_sample_scales_to_one_hundred() {
        let mut meter = LevelMeter::new(1);
        let bytes = samples_to_bytes(&[i16::MIN]);
        assert_eq!(meter.push(&bytes), vec![100]);
    }

    #[test]
    fn a_partial_batch_emits_nothing_until_completed() {
        let mut meter = LevelMeter::new(4);
        let bytes = samples_to_bytes(&[1, 2]);
        assert!(meter.push(&bytes).is_empty());
        let bytes = samples_to_bytes(&[3, 4]);
        assert_eq!(meter.push(&bytes), vec![0]);
    }

    #[test]
    fn one_call_can_emit_more_than_one_batch() {
        let mut meter = LevelMeter::new(2);
        let bytes = samples_to_bytes(&[0, 0, i16::MAX, i16::MAX]);
        assert_eq!(meter.push(&bytes), vec![0, (32767 * 100) / 32768]);
    }

    #[test]
    fn a_sample_split_across_two_reads_is_carried_over() {
        let mut meter = LevelMeter::new(1);
        let bytes = samples_to_bytes(&[i16::MIN]);
        assert!(meter.push(&bytes[..1]).is_empty());
        assert_eq!(meter.push(&bytes[1..]), vec![100]);
    }

    #[test]
    fn peak_resets_after_each_batch() {
        let mut meter = LevelMeter::new(2);
        let bytes = samples_to_bytes(&[i16::MAX, 0, 0, 0]);
        assert_eq!(meter.push(&bytes), vec![(32767 * 100) / 32768, 0]);
    }
}
