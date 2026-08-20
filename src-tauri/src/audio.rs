//! 麦克风采集（cpal）+ 重采样到 16kHz 单声道 f32。
//!
//! 本机实测（`examples/mic_probe.rs`）：MacBook Pro 麦克风仅支持
//! 44.1k/48k/88.2k/96k，**不支持 16kHz**；而 sherpa-onnx 流式模型要求
//! 16kHz 输入。因此本模块在采集后做线性插值重采样（任意采样率 → 16kHz），
//! 以 100ms（1600 采样）为块发送给下游（ASR sidecar）。
//!
//! 采样格式：优先 f32，其次 i16/u16（macOS 常见 F32）；其他格式返回错误。

use std::sync::mpsc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// 输出音频块大小：100ms @16kHz = 1600 采样。
pub const CHUNK_SAMPLES: usize = 1600;
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// 打开麦克风并把 16kHz 单声道 f32 音频块发送到 `tx`。
///
/// 返回 (采集句柄, 设备采样率)。句柄持有时流存活；drop 即停止采集。
///
/// 采样格式：优先 f32，其次 i16/u16（macOS 常见 F32）；其他格式返回错误。
pub fn start_mic_capture(tx: mpsc::Sender<Vec<f32>>) -> Result<(MicCapture, u32), String> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or("找不到默认输入设备（麦克风）")?;
    let config = device
        .default_input_config()
        .map_err(|e| format!("读取输入配置失败: {e}"))?;
    let src_rate = config.sample_rate();
    let channels = config.channels() as usize;

    let sample_format = config.sample_format();
    // cpal 0.18 的 DeviceTrait 没有 name() 方法，用 Display trait 获取设备名
    println!("[audio] 设备: {device}");

    println!("[audio] 采样格式: {sample_format:?}, 采样率: {src_rate}Hz, 通道数: {channels}");
    let stream = match sample_format {
        cpal::SampleFormat::F32 => build_stream::<f32>(&device, &config.into(), channels, src_rate, tx),
        cpal::SampleFormat::I16 => build_stream::<i16>(&device, &config.into(), channels, src_rate, tx),
        cpal::SampleFormat::U16 => build_stream::<u16>(&device, &config.into(), channels, src_rate, tx),
        other => Err(format!("不支持的采样格式: {other:?}")),
    }?;

    // cpal 流创建后处于暂停态，必须显式 play() 才开始回调。
    // （此前缺失导致回调零触发 → sidecar 收到静音 → 无识别文字。）
    stream
        .play()
        .map_err(|e| format!("启动麦克风流失败: {e}"))?;

    Ok((MicCapture { stream: Some(stream) }, src_rate))
}

/// 采集句柄：持有时麦克风流存活。
pub struct MicCapture {
    /// 仅在持有时才有效：字段本身不被读取（T13 托盘暂停/恢复时使用 pause/resume）。
    #[allow(dead_code)]
    stream: Option<cpal::Stream>,
}

impl MicCapture {
    /// 暂停采集（播放时继续）。T4 阶段仅保留接口（T13 托盘接入）。
    #[allow(dead_code)]
    pub fn pause(&mut self) {
        if let Some(s) = &self.stream {
            let _ = s.pause();
        }
    }

    /// 恢复采集。
    #[allow(dead_code)]
    pub fn resume(&mut self) {
        if let Some(s) = &self.stream {
            let _ = s.play();
        }
    }
}

/// 按采样格式泛型构建输入流：采集 → 下混单声道 → 重采样 16kHz → 分块发送。
fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    src_rate: u32,
    tx: mpsc::Sender<Vec<f32>>,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + cpal::FromSample<f32> + cpal::Sample,
    f64: cpal::FromSample<T>,
{
    let mut resampler = Resampler::new(src_rate, TARGET_SAMPLE_RATE, channels);
    let err_fn = |e| eprintln!("[audio] 采集流错误: {e}");

    device
        .build_input_stream(
            config.clone(),
            move |data: &[T], _| {
                for chunk in resampler.push(data) {
                    if tx.send(chunk).is_err() {
                        // 接收端已关闭，停止推送（流会自动结束）
                        break;
                    }
                }
            },
            err_fn,
            None,
        )
        .map_err(|e| format!("打开麦克风流失败: {e}"))
}

/// 线性插值重采样器：任意采样率 → 16kHz，多声道下混为单声道。
struct Resampler {
    src_rate: u32,
    dst_rate: u32,
    channels: usize,
    /// 未消费的输入缓冲（f32 单声道视角：下混已完成）。
    buf: Vec<f32>,
    /// 下次输出采样在 `buf` 中的浮点位置。
    pos: f64,
    /// 未满 100ms 的输出缓冲 —— **跨调用累积**（真实回调是小缓冲连续推入，
    /// 若为局部变量每次重建就永远凑不满一个块）。
    pending: Vec<f32>,
}

impl Resampler {
    fn new(src_rate: u32, dst_rate: u32, channels: usize) -> Self {
        Self {
            src_rate,
            dst_rate,
            channels,
            buf: Vec::with_capacity(src_rate as usize),
            pos: 0.0,
            pending: Vec::with_capacity(CHUNK_SAMPLES),
        }
    }

    /// 推入一帧采集数据，产出若干 100ms 的 16kHz 音频块。
    fn push<T>(&mut self, data: &[T]) -> Vec<Vec<f32>>
    where
        T: cpal::Sample,
        f64: cpal::FromSample<T>,
    {
        let mono_len = data.len() / self.channels;
        // 下混（平均所有声道）
        for i in 0..mono_len {
            let mut sum = 0.0f64;
            for c in 0..self.channels {
                sum += data[i * self.channels + c].to_sample::<f64>();
            }
            self.buf.push((sum / self.channels as f64) as f32);
        }

        let mut out = Vec::new();
        let step = self.src_rate as f64 / self.dst_rate as f64; // 每个输出采样消耗的输入采样数

        while (self.pos + step) <= self.buf.len() as f64 {
            let i0 = self.pos.floor() as usize;
            let frac = self.pos - i0 as f64;
            let s0 = self.buf[i0] as f64;
            let s1 = if i0 + 1 < self.buf.len() {
                self.buf[i0 + 1] as f64
            } else {
                s0
            };
            self.pending.push((s0 * (1.0 - frac) + s1 * frac) as f32);
            self.pos += step;

            // 用 >= 而非 ==：单次推入可能跨过 1600 的整数倍，余量留给下一个块
            if self.pending.len() >= CHUNK_SAMPLES {
                let rest = self.pending.split_off(CHUNK_SAMPLES);
                out.push(std::mem::replace(&mut self.pending, rest));
            }
        }

        // 回收已消费的缓冲（保留 pos 附近的 1 个采样以防作为 s1 使用）
        let keep_from = (self.pos.floor() as usize).saturating_sub(1);
        if keep_from > 0 {
            self.buf.drain(..keep_from);
            self.pos -= keep_from as f64;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_48k_to_16k_decimates_by_3() {
        let mut r = Resampler::new(48_000, 16_000, 1);
        // 30 个输入采样 → 10 个输出采样（pos 从 0 走到 30）
        let input: Vec<f32> = (0..30).map(|i| i as f32).collect();
        let out = r.push(&input);
        assert_eq!(out.len(), 0, "不足 100ms 不成块");
        assert_eq!(r.buf.len(), 1, "残留 1 个采样（pos 保留）");

        // 补足到能产出至少一个 100ms 块（1600 输出 × 3 = 4800 输入）
        let input2: Vec<f32> = (30..5100).map(|i| i as f32).collect();
        let out2 = r.push(&input2);
        let total: usize = out2.iter().map(|c| c.len()).sum();
        assert!(total >= CHUNK_SAMPLES, "应产出至少一个 100ms 块，got {total}");
    }

    #[test]
    fn resampler_identity_rate_passthrough() {
        let mut r = Resampler::new(16_000, 16_000, 1);
        let input: Vec<f32> = (0..1600).map(|i| i as f32).collect();
        let out = r.push(&input);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], input, "同采样率应原样通过");
    }

    #[test]
    fn resampler_downmixes_stereo() {
        let mut r = Resampler::new(16_000, 16_000, 2);
        // 左右声道交替：L=1.0, R=0.0 → 单声道 0.5
        let mut input = vec![0.0f32; 3200];
        for i in 0..1600 {
            input[i * 2] = 1.0;
        }
        let out = r.push(&input);
        assert_eq!(out.len(), 1);
        assert!(out[0].iter().all(|s| (*s - 0.5).abs() < 1e-6), "立体声下混为 0.5");
    }

    /// 回归：真实回调是**小缓冲连续推入**（512 帧 @48kHz ≈ 170 输出/次），
    /// 100ms 块必须**跨调用累积**。此前的 bug 是 chunk 为 push 局部变量，
    /// 每次调用重建，永远凑不满 1600 而零输出（静音 → 无识别文字）。
    #[test]
    fn resampler_accumulates_across_small_pushes() {
        let mut r = Resampler::new(48_000, 16_000, 1);
        let mut out: Vec<Vec<f32>> = Vec::new();
        // 模拟 12 次真实回调（每次 512 帧），累积应超过 1600 输出
        for _ in 0..12 {
            let input = vec![0.5f32; 512];
            out.extend(r.push(&input));
        }
        let total: usize = out.iter().map(|c| c.len()).sum();
        assert!(total >= CHUNK_SAMPLES, "跨回调累积应产出完整块，got {total}");
    }
}
