//! Streaming of long sounds: only the head stays in memory, the rest is
//! decoded from disk during playback, and what you hear must be identical to
//! playing a fully decoded copy.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use soundpad::audio::decode::{HEAD_SECS, STREAM_MIN_SECS, SoundData, decode_file, decode_file_with};
use soundpad::audio::mixer::{Command, mixer};

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("soundpad-stream-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_wav(path: &Path, rate: u32, channels: u16, samples: &[i16]) {
    let data_len = (samples.len() * 2) as u32;
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&channels.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
    b.extend_from_slice(&(channels * 2).to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        b.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, b).unwrap();
}

/// A sweep with different content per channel, so misaligned frames are caught.
fn sweep(frames: usize, rate: u32, channels: u16) -> Vec<i16> {
    let mut v = Vec::with_capacity(frames * channels as usize);
    for i in 0..frames {
        let t = i as f32 / rate as f32;
        for c in 0..channels {
            let hz = 200.0 + 30.0 * t + 100.0 * c as f32;
            v.push(((t * hz * std::f32::consts::TAU).sin() * 12000.0) as i16);
        }
    }
    v
}

/// Play `sound` through a mixer at `out_rate`, rendering slowly enough for
/// the streamer, and return (output with fully silent blocks removed, number
/// of silent blocks). A silent block means the stream fell behind.
fn play(sound: Arc<SoundData>, out_rate: u32) -> (Vec<f32>, usize) {
    let (mut m, mut h) = mixer(out_rate);
    h.send(Command::play(1, sound, false));
    let mut out = Vec::new();
    let mut block = vec![0.0f32; 512 * 2];
    let mut silent = 0;
    for _ in 0..100_000 {
        m.render(&mut block, 2);
        if block.iter().all(|&v| v == 0.0) {
            if h.status.active_voices.load(Relaxed) == 0 {
                break;
            }
            silent += 1;
        } else {
            out.extend_from_slice(&block);
        }
        h.collect_garbage();
        std::thread::sleep(Duration::from_micros(300));
    }
    assert_eq!(h.status.active_voices.load(Relaxed), 0, "voice never finished");
    (out, silent)
}

#[test]
fn long_sounds_keep_only_the_head_in_memory() {
    let d = tmpdir("head");
    let rate = 44_100;
    let p = d.join("long.wav");
    write_wav(&p, rate, 2, &sweep(rate as usize * 30, rate, 2));
    let s = decode_file(&p).unwrap();
    assert!(s.is_streamed());
    assert_eq!(s.frames(), rate as usize * 30);
    assert!((s.duration_secs() - 30.0).abs() < 1e-6);
    assert_eq!(s.head_frames(), (HEAD_SECS * rate as f64) as usize);
    assert_eq!(s.memory_bytes(), s.head_frames() * 4);

    // Short clips stay fully in memory.
    let p = d.join("short.wav");
    let n = (rate as f64 * STREAM_MIN_SECS) as usize - 100;
    write_wav(&p, rate, 2, &sweep(n, rate, 2));
    let s = decode_file(&p).unwrap();
    assert!(!s.is_streamed());
    assert_eq!((s.frames(), s.head_frames()), (n, n));
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn streamed_playback_is_identical_to_in_memory_playback() {
    let d = tmpdir("same");
    let rate = 44_100;
    for channels in [2u16, 1] {
        let p = d.join(format!("song{channels}.wav"));
        write_wav(&p, rate, channels, &sweep(rate as usize * 12, rate, channels));
        let streamed = Arc::new(decode_file(&p).unwrap());
        let full = Arc::new(decode_file_with(&p, f64::INFINITY).unwrap());
        assert!(streamed.is_streamed() && !full.is_streamed());
        assert_eq!(streamed.channels as u16, channels);
        // Same rate (direct copy) and resampled to 48 kHz (cubic interpolation).
        for out_rate in [44_100, 48_000] {
            let (a, silent) = play(streamed.clone(), out_rate);
            let (b, _) = play(full.clone(), out_rate);
            assert!(silent <= 2, "stream fell behind {silent} times ({channels} ch, {out_rate} Hz)");
            assert_eq!(a.len(), b.len(), "{channels} ch, {out_rate} Hz");
            let diff = a.iter().zip(&b).position(|(x, y)| x != y);
            assert_eq!(diff, None, "{channels} ch, {out_rate} Hz: first difference");
        }
    }
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn deleted_file_ends_playback_after_the_head() {
    let d = tmpdir("gone");
    let rate = 44_100;
    let p = d.join("song.wav");
    write_wav(&p, rate, 2, &sweep(rate as usize * 12, rate, 2));
    let s = Arc::new(decode_file(&p).unwrap());
    std::fs::remove_file(&p).unwrap();
    let (out, _) = play(s.clone(), rate);
    // The in-memory head still plays, then the voice ends instead of hanging.
    assert_eq!(out.len() / 2, s.head_frames().div_ceil(512) * 512);
    std::fs::remove_dir_all(d).ok();
}

/// Render `sound` after `setup` until `frames` stereo frames of non-silent
/// output were collected.
fn collect(m: &mut soundpad::audio::mixer::Mixer, frames: usize) -> Vec<f32> {
    let mut out = Vec::new();
    let mut block = vec![0.0f32; 256 * 2];
    for _ in 0..100_000 {
        m.render(&mut block, 2);
        if block.iter().any(|&v| v != 0.0) {
            out.extend_from_slice(&block);
        }
        if out.len() >= frames * 2 {
            break;
        }
        std::thread::sleep(Duration::from_micros(300));
    }
    out
}

#[test]
fn streamed_seek_near_and_far() {
    let d = tmpdir("seek");
    let rate = 48_000;
    let p = d.join("song.wav");
    write_wav(&p, rate, 2, &sweep(rate as usize * 40, rate, 2));
    let streamed = Arc::new(decode_file(&p).unwrap());
    let full = decode_file_with(&p, f64::INFINITY).unwrap();
    // 5 s is decoded up to (bit-exact); 30 s jumps in the file.
    for secs in [5usize, 30] {
        let (mut m, mut h) = mixer(rate);
        h.send(Command::play(1, streamed.clone(), true));
        collect(&mut m, 512);
        let target = secs * rate as usize;
        h.send(Command::seek(1, &streamed, target, false));
        let out = collect(&mut m, 4096);
        // After the 6 ms fade-in the audio is exactly the file from `target`.
        for f in 1000..4000 {
            for c in 0..2 {
                assert_eq!(out[f * 2 + c], full.sample(target + f, c), "{secs} s, frame {f}");
            }
        }
        h.collect_garbage();
    }
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn streamed_loop_restarts_at_the_beginning() {
    let d = tmpdir("loop");
    let rate = 48_000;
    let p = d.join("song.wav");
    let frames = rate as usize * 11;
    write_wav(&p, rate, 2, &sweep(frames, rate, 2));
    let streamed = Arc::new(decode_file(&p).unwrap());
    assert!(streamed.is_streamed());
    let full = decode_file_with(&p, f64::INFINITY).unwrap();
    let (mut m, mut h) = mixer(rate);
    h.send(Command::play_with(1, streamed.clone(), true, true));
    let out = collect(&mut m, frames + rate as usize * 3);
    for f in (0..frames + rate as usize * 3).step_by(997) {
        assert_eq!(out[f * 2], full.sample(f % frames, 0), "frame {f}");
    }
    assert_eq!(h.status.active_voices.load(Relaxed), 1);
    assert_eq!(h.status.ended_count.load(Relaxed), 0);
    h.send(Command::StopAll);
    std::fs::remove_dir_all(d).ok();
}
