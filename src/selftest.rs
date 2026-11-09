//! `soundpad --selftest`: drive the real CoreAudio devices (silently) and
//! measure callback cost, missed callbacks and trigger→audio latency.

use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

use crate::audio::decode::SoundData;
use crate::audio::engine::{self, AudioEngine};
use crate::audio::player::{OutputKind, Player};

fn tone(secs: f32, rate: u32, hz: f32) -> Arc<SoundData> {
    let n = (secs * rate as f32) as usize;
    let mut v = Vec::with_capacity(n * 2);
    for i in 0..n {
        let s = (i as f32 * hz * std::f32::consts::TAU / rate as f32).sin() * 0.5;
        v.push(s);
        v.push(s);
    }
    Arc::new(SoundData::new(v, rate))
}

pub fn run() -> i32 {
    let player = Player::new();
    let mut eng = AudioEngine::new(player.clone());
    // Silent: everything is rendered, nothing is audible.
    player.set_volume(OutputKind::Speakers, 0.0);
    player.set_volume(OutputKind::VirtualMic, 0.0);
    player.set_passthrough_volume(0.0);
    player.overlap.store(true, Relaxed);

    println!("Output devices:");
    for d in engine::output_devices() {
        println!("  {}{}", d.name, if d.looks_virtual() { "  [virtual]" } else { "" });
    }
    if let Err(e) = eng.set_output(OutputKind::Speakers, Some("")) {
        println!("FAIL: cannot open default output: {e}");
        return 1;
    }
    // Second output: a virtual cable if present, otherwise any other output, so
    // two streams on independent clocks run simultaneously like in real use.
    let outs = engine::output_devices();
    let default = engine::default_output_id();
    let virtual_dev = crate::virtual_mic::find_device(&outs).or_else(|| outs.iter().find(|d| d.looks_virtual())).cloned();
    let second = virtual_dev
        .as_ref()
        .or_else(|| outs.iter().find(|d| Some(&d.id) != default.as_ref()));
    if let Some(v) = second {
        match eng.set_output(OutputKind::VirtualMic, Some(&v.id)) {
            Ok(()) => println!("Second output (virtual-mic path): {}", v.name),
            Err(e) => println!("note: second output failed: {e}"),
        }
    }

    // Different source rates exercise the resampler.
    for (id, rate) in [(1u64, 44_100u32), (2, 48_000), (3, 22_050), (4, 96_000)] {
        player.insert_sound(id, tone(30.0, rate, 220.0 * id as f32));
    }

    let mut ok = true;
    for k in [OutputKind::Speakers, OutputKind::VirtualMic] {
        let Some(out) = eng.output(k) else { continue };
        let st = player.output_status(k).unwrap();
        let period = out.buffer_frames.unwrap_or(512) as f64 / out.sample_rate as f64;
        println!(
            "\n[{:?}] {} — {} Hz, {} ch, buffer {} frames ({:.1} ms)",
            k,
            out.device.name,
            out.sample_rate,
            out.channels,
            out.buffer_frames.map(|b| b.to_string()).unwrap_or_else(|| "default".into()),
            period * 1000.0
        );

        // Trigger latency: command sent -> first callback that renders it.
        let mut lat = Vec::new();
        for i in 0..20 {
            player.stop_all();
            std::thread::sleep(Duration::from_millis(30));
            let t = Instant::now();
            player.play(1 + (i % 4));
            while st.sound_id.load(Relaxed) == 0 && t.elapsed() < Duration::from_secs(1) {
                std::hint::spin_loop();
            }
            lat.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        lat.sort_by(f64::total_cmp);
        let p50 = lat[lat.len() / 2];
        let max = *lat.last().unwrap();
        println!("  trigger→render latency: median {p50:.2} ms, max {max:.2} ms (+ device output latency)");
        if max > 50.0 {
            ok = false;
            println!("  FAIL: trigger latency too high");
        }

        // Stress: 40 overlapping voices with resampling for 3 seconds.
        st.max_callback_ns.store(0, Relaxed);
        let cb0 = st.callbacks.load(Relaxed);
        let slow0 = st.slow_callbacks.load(Relaxed);
        let t0 = Instant::now();
        for i in 0..40 {
            player.play(1 + (i % 4));
        }
        std::thread::sleep(Duration::from_secs(3));
        let secs = t0.elapsed().as_secs_f64();
        let cbs = st.callbacks.load(Relaxed) - cb0;
        let slow = st.slow_callbacks.load(Relaxed) - slow0;
        // Use the buffer size the device really delivers, not the one we asked for.
        let frames = st.callback_frames.load(Relaxed).max(1) as f64;
        let period = frames / out.sample_rate as f64;
        let expected = secs / period;
        let max_ns = st.max_callback_ns.load(Relaxed) as f64;
        let load = max_ns / (period * 1e9) * 100.0;
        println!(
            "  stress (40 voices): {cbs} callbacks of {frames:.0} frames in {secs:.2}s (expected ≈{expected:.0}), \
             worst {:.3} ms = {load:.1}% of budget, callbacks over 50%: {slow}, voices {}",
            max_ns / 1e6,
            st.active_voices.load(Relaxed)
        );
        if (cbs as f64) < expected * 0.9 {
            ok = false;
            println!("  FAIL: callbacks missing (device stalled)");
        }
        // A single scheduler hiccup is not a glitch; a deadline miss or a pattern is.
        if load > 100.0 || slow as f64 > cbs as f64 * 0.01 {
            ok = false;
            println!("  FAIL: audio callback too slow");
        } else if slow > 0 {
            println!("  note: {slow} isolated slow callback(s), still within the deadline");
        }
        player.stop_all();
        std::thread::sleep(Duration::from_millis(50));
        player.collect_garbage();
    }

    if let Some(v) = &virtual_dev {
        ok &= loopback_test(&player, &mut eng, v);
    } else {
        println!("\n[Loopback] skipped: no virtual mic device installed");
    }
    if std::env::args().any(|a| a == "--with-mic") {
        ok &= mic_passthrough_test(&player, &mut eng);
    }

    println!("\n{}", if ok { "SELFTEST PASSED" } else { "SELFTEST FAILED" });
    if ok { 0 } else { 1 }
}

/// Record audio from an input device (test harness only, not real-time critical).
struct Recorder {
    _stream: cpal::Stream,
    rate: u32,
    /// (arrival time, mono samples)
    chunks: Arc<parking_lot::Mutex<Vec<(Instant, Vec<f32>)>>>,
}

fn record(device_name: &str) -> Result<Recorder, String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let host = cpal::default_host();
    let dev = host
        .input_devices()
        .map_err(|e| e.to_string())?
        .find(|d| d.description().map(|x| x.name() == device_name).unwrap_or(false))
        .ok_or_else(|| format!("no input side for {device_name}"))?;
    let cfg = dev.default_input_config().map_err(|e| e.to_string())?;
    if cfg.sample_format() != cpal::SampleFormat::F32 {
        return Err(format!("unexpected input format {}", cfg.sample_format()));
    }
    let rate = cfg.sample_rate();
    let ch = cfg.channels() as usize;
    let chunks: Arc<parking_lot::Mutex<Vec<(Instant, Vec<f32>)>>> = Arc::default();
    let sink = chunks.clone();
    let stream = dev
        .build_input_stream::<f32, _, _>(
            cfg.config(),
            move |data: &[f32], _| {
                let mono: Vec<f32> = data.chunks(ch).map(|f| f[0]).collect();
                sink.lock().push((Instant::now(), mono));
            },
            |e| eprintln!("recorder error: {e}"),
            None,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(Recorder { _stream: stream, rate, chunks })
}

/// Goertzel power of `hz` in `x`.
fn tone_power(x: &[f32], rate: u32, hz: f32) -> f32 {
    let w = std::f32::consts::TAU * hz / rate as f32;
    let c = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for &v in x {
        let s0 = v + c * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    (s1 * s1 + s2 * s2 - c * s1 * s2) / (x.len() as f32 * x.len() as f32)
}

/// End-to-end: play a tone into the virtual mic output and capture it from the
/// virtual mic's input side — exactly what Discord/Zoom would receive.
fn loopback_test(player: &Arc<Player>, eng: &mut AudioEngine, dev: &crate::audio::engine::DeviceInfo) -> bool {
    println!("\n[Loopback] {} → output → driver → input (what other apps hear)", dev.name);
    if eng.output(OutputKind::VirtualMic).map(|o| o.device.id.as_str()) != Some(dev.id.as_str()) {
        if let Err(e) = eng.set_output(OutputKind::VirtualMic, Some(&dev.id)) {
            println!("  FAIL: cannot open {}: {e}", dev.name);
            return false;
        }
    }
    let rec = match record(crate::virtual_mic::recording_side(&dev.name)) {
        Ok(r) => r,
        Err(e) => {
            println!("  FAIL: {e}");
            return false;
        }
    };
    let out_rate = eng.output(OutputKind::VirtualMic).unwrap().sample_rate;
    player.stop_all();
    // Only the virtual device gets sound; speakers stay silent.
    player.set_output_enabled(OutputKind::VirtualMic, true);
    player.set_volume(OutputKind::VirtualMic, 1.0); // gain 1.0
    player.set_volume(OutputKind::Speakers, 0.0);
    player.overlap.store(false, Relaxed);
    player.insert_sound(100, tone(1.5, 44_100, 1000.0)); // amplitude 0.5, needs resampling
    std::thread::sleep(Duration::from_millis(400));
    rec.chunks.lock().clear();

    let t0 = Instant::now();
    player.play(100);
    std::thread::sleep(Duration::from_millis(1900));
    player.set_volume(OutputKind::VirtualMic, 0.0);

    let chunks = std::mem::take(&mut *rec.chunks.lock());
    let samples: Vec<f32> = chunks.iter().flat_map(|(_, c)| c.iter().copied()).collect();
    let Some(onset) = chunks
        .iter()
        .find(|(_, c)| c.iter().any(|v| v.abs() > 0.05))
        .map(|(t, _)| t.saturating_duration_since(t0))
    else {
        let silent = samples.iter().all(|&v| v == 0.0);
        println!(
            "  FAIL: no signal captured{}",
            if silent { " (all zeros — is microphone access allowed for this terminal/app?)" } else { "" }
        );
        return false;
    };
    let start = samples.iter().position(|v| v.abs() > 0.05).unwrap();
    let rate = rec.rate;
    // Analyse the steady middle of the tone (skip 50 ms fade/onset each side).
    let a = start + rate as usize / 20;
    let b = (start + rate as usize * 145 / 100).min(samples.len());
    let body = &samples[a..b];
    let rms = (body.iter().map(|v| v * v).sum::<f32>() / body.len() as f32).sqrt();
    let p1k = tone_power(body, rate, 1000.0);
    let p_off = tone_power(body, rate, 1370.0) + tone_power(body, rate, 700.0);
    // Dropout check: RMS of every 10 ms window must stay near the overall RMS.
    let win = rate as usize / 100;
    let worst = body
        .chunks_exact(win)
        .map(|w| (w.iter().map(|v| v * v).sum::<f32>() / w.len() as f32).sqrt())
        .map(|r| (r / rms - 1.0).abs())
        .fold(0.0f32, f32::max);
    let expected_rms = 0.5 / 2f32.sqrt();
    println!(
        "  device {} Hz (sound 44.1 kHz resampled to {} Hz) · onset after {:.1} ms (incl. driver + capture buffers)",
        rate,
        out_rate,
        onset.as_secs_f64() * 1000.0
    );
    println!(
        "  level: rms {:.3} (expected {:.3}) · 1 kHz vs off-frequency energy: {:.0} dB · worst 10 ms window deviation {:.1}%",
        rms,
        expected_rms,
        10.0 * (p1k / p_off.max(1e-12)).log10(),
        worst * 100.0
    );
    let mut ok = true;
    if (rms - expected_rms).abs() > 0.05 {
        println!("  FAIL: level wrong");
        ok = false;
    }
    if p1k < p_off * 1000.0 {
        println!("  FAIL: tone is distorted / wrong pitch");
        ok = false;
    }
    if worst > 0.1 {
        println!("  FAIL: dropout or glitch detected");
        ok = false;
    }
    if onset > Duration::from_millis(100) {
        println!("  FAIL: latency too high");
        ok = false;
    }
    if ok {
        println!("  OK: clean, correct-pitch, gap-free audio arrives on the virtual mic");
    }
    ok
}

/// Real microphone → mixer → virtual mic. Tries the default input first, then
/// other physical inputs (e.g. a closed MacBook lid makes the built-in mic silent).
fn mic_passthrough_test(player: &Arc<Player>, eng: &mut AudioEngine) -> bool {
    println!("\n[Mic passthrough] microphone → mixer → virtual mic output");
    if eng.output(OutputKind::VirtualMic).is_none() {
        println!("  skipped: no virtual mic output open");
        return true;
    }
    player.set_passthrough_volume(0.0); // measure without sending it anywhere audible
    let mut candidates = vec![String::new()]; // "" = system default
    let default = engine::default_input_id();
    candidates.extend(
        engine::input_devices()
            .into_iter()
            .filter(|d| !d.looks_virtual() && !crate::virtual_mic::is_virtual_mic(d) && Some(&d.id) != default.as_ref())
            .filter(|d| !d.name.contains("Teams"))
            .map(|d| d.id),
    );
    let mut any_ok = false;
    let mut pipeline_ok = true;
    for id in candidates {
        if let Err(e) = eng.set_input(Some(&id)) {
            println!("  {}: cannot open: {e}", if id.is_empty() { "default" } else { &id });
            continue;
        }
        let name = eng.input_device().map(|d| d.name.clone()).unwrap_or_default();
        let st = player.output_status(OutputKind::VirtualMic).unwrap();
        let f0 = st.mic_frames.load(Relaxed);
        let u0 = st.mic_underruns.load(Relaxed);
        let mut peak = 0.0f32;
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(20));
            peak = peak.max(st.mic_peak.get());
        }
        let secs = t.elapsed().as_secs_f64();
        let frames = st.mic_frames.load(Relaxed) - f0;
        let underruns = st.mic_underruns.load(Relaxed) - u0;
        let out_rate = eng.output(OutputKind::VirtualMic).unwrap().sample_rate as f64;
        let in_rate = eng.input_rate().unwrap_or(0);
        // Allow for the initial ~15 ms priming.
        let coverage = frames as f64 / (secs * out_rate) * 100.0;
        let silent = peak == 0.0;
        println!(
            "  {name}{}: {in_rate} Hz → {out_rate:.0} Hz · audio in {coverage:.1}% of output frames · underruns {underruns} · peak {:.1} dBFS{}",
            if id.is_empty() { " (default)" } else { "" },
            20.0 * peak.max(1e-6).log10(),
            if silent { "  ← digital silence (muted/closed-lid mic or no permission)" } else { "" }
        );
        if coverage < 98.0 || underruns > 0 {
            pipeline_ok = false;
            println!("  FAIL: mic audio not flowing continuously");
        }
        if !silent {
            any_ok = true;
            break;
        }
    }
    let _ = eng.set_input(None);
    if !any_ok {
        println!("  FAIL: no microphone delivered any sound");
    } else if pipeline_ok {
        println!("  OK: live mic is mixed continuously into the virtual mic");
    }
    any_ok && pipeline_ok
}
