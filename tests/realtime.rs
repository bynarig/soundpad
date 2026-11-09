//! Real-time safety and performance of the mixer.
//!
//! 1. The audio callback must never allocate or free memory (that can block
//!    on a lock inside the allocator and cause audible dropouts).
//! 2. The callback must be far cheaper than its time budget even with many
//!    overlapping, resampled voices plus live microphone input.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::time::Instant;

use ringbuf::traits::Producer;
use soundpad::audio::decode::SoundData;
use soundpad::audio::mixer::{Command, MAX_VOICES, MicInput, mixer};

struct CountingAlloc;

thread_local! {
    static TRACK: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

fn bump() {
    // try_with: TLS may be gone during thread teardown.
    let _ = TRACK.try_with(|t| {
        if t.get() {
            let _ = COUNT.try_with(|c| c.set(c.get() + 1));
        }
    });
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        bump();
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        bump();
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        bump();
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static A: CountingAlloc = CountingAlloc;

/// Count allocations made by `f` on this thread.
fn allocs_during(f: impl FnOnce()) -> usize {
    COUNT.with(|c| c.set(0));
    TRACK.with(|t| t.set(true));
    f();
    TRACK.with(|t| t.set(false));
    COUNT.with(|c| c.get())
}

fn tone(frames: usize, rate: u32) -> Arc<SoundData> {
    let v: Vec<f32> = (0..frames * 2).map(|i| ((i / 2) as f32 * 0.05).sin() * 0.3).collect();
    Arc::new(SoundData::new(v, rate))
}

#[test]
fn audio_callback_never_allocates() {
    let (mut m, mut h) = mixer(48_000);
    let (mut mic_prod, mic) = MicInput::new(44_100, 48_000);
    let sounds = [tone(2000, 44_100), tone(3000, 48_000), tone(500, 22_050)];
    let mut out = vec![0.0f32; 512 * 2];

    // Sanity check: the counter really sees allocations.
    assert!(allocs_during(|| drop(std::hint::black_box(vec![1u8; 64]))) >= 2);

    // Warm-up render (first call may legitimately size internal buffers).
    m.render(&mut out, 2);

    // Commands are queued from "the UI thread" (allocation allowed there)...
    h.send(Command::SetMicInput(Some(mic)));
    for i in 0..(MAX_VOICES + 20) {
        h.send(Command::play(i as u64, sounds[i % 3].clone(), false));
    }
    mic_prod.push_slice(&vec![0.1f32; 44_100]);

    // ...but every render — handling the commands, stealing voices, finishing
    // sounds, fading, mixing the mic, swapping the mic input — must not allocate.
    let n = allocs_during(|| {
        for _ in 0..400 {
            m.render(&mut out, 2);
        }
    });
    assert_eq!(n, 0, "render() allocated {n} times");

    h.send(Command::StopAll);
    h.send(Command::SetMicInput(None)); // old mic must go to the garbage queue
    h.send(Command::play(1, sounds[0].clone(), true));
    h.send(Command::SetPaused(true));
    let n = allocs_during(|| {
        for _ in 0..100 {
            m.render(&mut out, 2);
        }
    });
    assert_eq!(n, 0, "render() allocated {n} times after stop/mic swap");

    // Memory is reclaimed on the control thread.
    h.collect_garbage();
}

#[test]
fn many_voices_fit_easily_in_time_budget() {
    // 48 kHz, 256-frame buffers => 5.33 ms per callback. Use the heaviest
    // case: every voice needs cubic resampling, plus mic passthrough.
    let rate = 48_000;
    let frames = 256;
    let budget_ns = frames as f64 / rate as f64 * 1e9;
    let (mut m, mut h) = mixer(rate);
    let (mut mic_prod, mic) = MicInput::new(44_100, rate);
    h.send(Command::SetMicInput(Some(mic)));
    let s = tone(44_100 * 60, 44_100);
    for i in 0..MAX_VOICES {
        h.send(Command::play(i as u64, s.clone(), false));
    }
    let mut out = vec![0.0f32; frames * 2];
    let mic_chunk = vec![0.05f32; 236 * 2];
    let mut worst = 0u128;
    let mut total = 0u128;
    let iters = 2000;
    for _ in 0..iters {
        mic_prod.push_slice(&mic_chunk);
        let t = Instant::now();
        m.render(&mut out, 2);
        let e = t.elapsed().as_nanos();
        worst = worst.max(e);
        total += e;
    }
    let avg = total as f64 / iters as f64;
    eprintln!(
        "{MAX_VOICES} resampled voices + mic, {frames} frames: avg {:.1} µs ({:.2}% of budget), worst {:.1} µs",
        avg / 1e3,
        avg / budget_ns * 100.0,
        worst as f64 / 1e3
    );
    // Generous bound so the test is stable on loaded CI machines.
    assert!(avg < budget_ns * 0.25, "mixing too slow: avg {avg} ns vs budget {budget_ns} ns");
}

#[test]
fn streaming_a_long_sound_never_allocates_in_the_callback() {
    // 20 s stereo WAV: long enough to be streamed from disk.
    let rate = 44_100u32;
    let frames = rate as usize * 20;
    let dir = std::env::temp_dir().join(format!("soundpad-rt-stream-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("long.wav");
    let data_len = (frames * 4) as u32;
    let mut b = Vec::with_capacity(44 + frames * 4);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    for v in [16u32.to_le_bytes(), [1, 0, 2, 0], rate.to_le_bytes(), (rate * 4).to_le_bytes(), [4, 0, 16, 0]] {
        b.extend_from_slice(&v);
    }
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..frames {
        let s = (((i as f32) * 0.03).sin() * 9000.0) as i16;
        b.extend_from_slice(&s.to_le_bytes());
        b.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(&path, b).unwrap();

    let sound = Arc::new(soundpad::audio::decode::decode_file(&path).unwrap());
    assert!(sound.is_streamed());
    let (mut m, mut h) = mixer(48_000);
    let mut out = vec![0.0f32; 512 * 2];
    m.render(&mut out, 2);
    // Two voices of the same song (resampled), as with speakers + virtual mic overlap.
    h.send(Command::play(1, sound.clone(), false));
    h.send(Command::play(2, sound.clone(), false));
    // Render past the in-memory head into the streamed part, seek, loop, then stop.
    let mut n = 0;
    for i in 0..400 {
        match i {
            // Seek both near (decoded up to) and far (jump in the file), toggle loop.
            150 => drop(h.send(Command::seek(1, &sound, 44_100 * 3, false))),
            200 => drop(h.send(Command::seek(2, &sound, 44_100 * 17, true))),
            220 => drop(h.send(Command::SetLooping(true))),
            300 => drop(h.send(Command::StopAll)),
            _ => {}
        }
        n += allocs_during(|| m.render(&mut out, 2));
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(n, 0, "render() allocated {n} times while streaming");
    h.collect_garbage();
    std::fs::remove_dir_all(dir).ok();
}
