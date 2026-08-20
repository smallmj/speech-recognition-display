//! 麦克风采集探针：用应用真实的 `audio::start_mic_capture` 采 3 秒，
//! 打印采样数/峰值/RMS 能量并把重采样后的 16kHz 音频落盘为 wav。
//!
//! 用途（诊断回路 B）：
//! - RMS 接近 0 → 麦克风被系统静音 / 权限未授予 / 采集到静音；
//! - RMS 正常 → 用 `sherpa_streaming.py --wav` 识别该 wav，若也无声则查重采样器。

use std::sync::mpsc;
use std::time::{Duration, Instant};

use talksee_lib::audio::start_mic_capture;

fn main() {
    let (tx, rx) = mpsc::channel();
    let (mut mic, src_rate) = match start_mic_capture(tx) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("CAPTURE-ERR: {e}");
            std::process::exit(1);
        }
    };
    println!("capture started, src_rate={src_rate}");
    // 诊断探测（H1）：显式启动流 —— 若 samples 从 0 变正，即证实 play() 缺失是根因。
    mic.resume();

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut samples: Vec<f32> = Vec::new();
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => samples.extend_from_slice(&chunk),
            Err(_) => break,
        }
    }
    drop(mic);

    let n = samples.len();
    let peak = samples.iter().fold(0.0f32, |a, s| a.max(s.abs()));
    let rms = if n > 0 {
        (samples.iter().map(|s| s * s).sum::<f32>() / n as f32).sqrt()
    } else {
        0.0
    };
    println!("samples={n} peak={peak:.6} rms={rms:.6}");

    let path = "/tmp/mic_dump.wav";
    write_wav_16k(path, &samples);
    println!("wav written: {path}");
}

/// 写 16kHz 单声道 16-bit PCM wav。
fn write_wav_16k(path: &str, samples: &[f32]) {
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    let data_len = (samples.len() * 2) as u32;
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&16_000u32.to_le_bytes());
    out.extend_from_slice(&32_000u32.to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(path, out).expect("write wav");
}
