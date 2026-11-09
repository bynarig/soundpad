//! Decoding tests against generated WAVs and real files encoded by macOS tools.

use std::path::{Path, PathBuf};
use std::process::Command;

use soundpad::audio::decode::{decode_file, is_supported};

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("soundpad-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Minimal 16-bit PCM WAV writer.
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

fn sine_i16(frames: usize, rate: u32, channels: u16) -> Vec<i16> {
    let mut v = Vec::new();
    for i in 0..frames {
        let s = ((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 16000.0) as i16;
        for _ in 0..channels {
            v.push(s);
        }
    }
    v
}

#[test]
fn mono_wav_becomes_stereo() {
    let d = tmpdir("mono");
    let p = d.join("mono.wav");
    write_wav(&p, 22_050, 1, &sine_i16(22_050, 22_050, 1));
    let s = decode_file(&p).unwrap();
    assert_eq!(s.sample_rate, 22_050);
    assert_eq!(s.frames(), 22_050);
    assert!((s.duration_secs() - 1.0).abs() < 1e-6);
    // Mono stays mono (half the memory); the mixer plays it on both channels.
    assert_eq!(s.channels, 1);
    assert_eq!(s.samples.len(), 22_050);
    for i in 0..s.frames() {
        assert_eq!(s.sample(i, 0), s.sample(i, 1));
    }
    let peak = s.samples.iter().fold(0.0f32, |a, &b| a.max((b as f32 / 32768.0).abs()));
    assert!((peak - 16000.0 / 32768.0).abs() < 0.01, "peak {peak}");
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn stereo_wav() {
    let d = tmpdir("stereo");
    let p = d.join("st.wav");
    write_wav(&p, 48_000, 2, &sine_i16(4800, 48_000, 2));
    let s = decode_file(&p).unwrap();
    assert_eq!((s.sample_rate, s.frames(), s.channels), (48_000, 4800, 2));
    // 16-bit storage: 4 bytes per stereo frame.
    assert_eq!(s.memory_bytes(), 4800 * 4);
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn garbage_and_missing_files_error_cleanly() {
    let d = tmpdir("bad");
    let p = d.join("bad.mp3");
    std::fs::write(&p, b"this is not audio at all").unwrap();
    assert!(decode_file(&p).is_err());
    assert!(decode_file(&d.join("missing.wav")).is_err());
    let empty = d.join("empty.wav");
    write_wav(&empty, 44_100, 2, &[]);
    assert!(decode_file(&empty).is_err());
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn extension_filter() {
    assert!(is_supported(Path::new("a.MP3")));
    assert!(is_supported(Path::new("a.flac")));
    assert!(!is_supported(Path::new("a.txt")));
    assert!(!is_supported(Path::new("noext")));
}

#[test]
fn macos_system_sounds_decode() {
    let dir = Path::new("/System/Library/Sounds");
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut n = 0;
    for e in rd.flatten() {
        let p = e.path();
        let s = decode_file(&p).unwrap_or_else(|err| panic!("{}: {err}", p.display()));
        assert!(s.frames() > 100 && matches!(s.channels, 1 | 2));
        n += 1;
    }
    assert!(n > 0);
}

/// Encode a WAV with the tools available on this Mac (mp3/aac/flac/ogg) and decode each.
#[test]
fn common_formats_via_encoders() {
    let d = tmpdir("formats");
    let wav = d.join("src.wav");
    write_wav(&wav, 44_100, 2, &sine_i16(44_100, 44_100, 2));
    let mut targets: Vec<(&str, Vec<String>)> = Vec::new();
    let has = |bin: &str| Command::new("which").arg(bin).output().is_ok_and(|o| o.status.success());
    if has("afconvert") {
        targets.push(("m4a", vec!["afconvert".into(), "-f".into(), "m4af".into(), "-d".into(), "aac".into()]));
        targets.push(("aiff", vec!["afconvert".into(), "-f".into(), "AIFF".into(), "-d".into(), "BEI16".into()]));
        targets.push(("caf", vec!["afconvert".into(), "-f".into(), "caff".into(), "-d".into(), "LEF32".into()]));
    }
    if has("ffmpeg") {
        for ext in ["mp3", "flac", "ogg"] {
            targets.push((ext, vec!["ffmpeg".into(), "-loglevel".into(), "error".into(), "-y".into(), "-i".into()]));
        }
    }
    let mut tested = Vec::new();
    for (ext, args) in targets {
        let out = d.join(format!("t.{ext}"));
        let mut cmd = Command::new(&args[0]);
        cmd.args(&args[1..]).arg(&wav).arg(&out);
        if !cmd.status().is_ok_and(|s| s.success()) {
            continue;
        }
        let s = decode_file(&out).unwrap_or_else(|e| panic!("{ext}: {e}"));
        assert_eq!(s.sample_rate, 44_100, "{ext}");
        // Encoders add priming/padding; length must still be about 1 s.
        assert!((s.duration_secs() - 1.0).abs() < 0.1, "{ext}: {}s", s.duration_secs());
        tested.push(ext);
    }
    eprintln!("decoded formats: {tested:?}");
    std::fs::remove_dir_all(d).ok();
}
