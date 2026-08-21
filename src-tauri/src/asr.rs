//! 真实 ASR：麦克风采集 + sherpa-onnx Python sidecar。
//!
//! 数据通路：
//! ```text
//! 麦克风 (cpal, 48kHz 等) → 重采样 16kHz → stdin 二进制 f32
//!   → sherpa_streaming.py（sherpa-onnx 流式识别）
//!   → stdout NDJSON：partial（边说边出）/ final（一句话定稿）
//!   → finals 缓冲为 Utterance，由 Engine::step 拉取；partials 由壳层轮询后 publish
//! ```
//!
//! T5 起接入说话人切换检测（SCD）：final 事件若携带该段音频的 speaker embedding
//! （sidecar 配置了 speaker embedding 模型），则经 [engine::Scd] 余弦匹配决定
//! speaker_id / gender / is_new_speaker；未配置模型时降级为单说话人（全部
//! speaker_id=1，注释「SCD 降级为单说话人」，见 [read_stdout]）。
//! 路径解析基于 `CARGO_MANIFEST_DIR`（即 `src-tauri/`），sidecar 与模型在开发期
//! 存放于该目录下；打包分发方案延后（用户决定先跑通流程）。

use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use engine::Scd;
use engine::{AsrPort, Gender, Utterance};
use tauri::{AppHandle, Manager};

/// sidecar 状态：0=启动中 1=已启动 2=出错 3=已停止
const ST_STARTING: u8 = 0;
const ST_STARTED: u8 = 1;
const ST_ERROR: u8 = 2;
const ST_STOPPED: u8 = 3;

/// 模型目录环境变量覆盖（调试/CI 用）。
pub const MODEL_DIR_ENV: &str = "SHERPA_MODEL_DIR";

/// 说话人 speaker embedding 模型目录环境变量覆盖（T5 SCD；`asr-models/` 下自动探测兜底）。
pub const EMBEDDING_MODEL_DIR_ENV: &str = "SHERPA_EMBEDDING_MODEL_DIR";

/// 真实 ASR（sherpa-onnx sidecar）。实现 [AsrPort]。
pub struct SherpaAsr {
    child: Child,
    /// 已定稿句子缓冲（Engine::next_utterance 拉取）。
    finals: Arc<Mutex<VecDeque<Utterance>>>,
    /// 实时 partial 缓冲（壳层轮询 publish 给前端）。
    partials: Arc<Mutex<VecDeque<String>>>,
    status: Arc<AtomicU8>,
    last_error: Arc<Mutex<Option<String>>>,
    /// sidecar 是否已在 started 事件中确认加载了 speaker embedding 模型
    /// （真实 embedding 路径；SCD 状态本身由 stdout 读线程独占持有）。
    scd_embedding: Arc<AtomicBool>,
    /// spawn 时是否向 sidecar 传了 `--embedding-model-dir`（配置层面是否启用 SCD）。
    scd_configured: bool,
    /// 麦克风句柄：持有时采集流存活（本身不被读取）。
    #[allow(dead_code)]
    mic: crate::audio::MicCapture,
}

impl SherpaAsr {
    /// 启动 sidecar + 麦克风采集。失败返回错误原因（壳层回退到 mock）。
    pub fn spawn(handle: &AppHandle) -> Result<Self, String> {
        let model_dir = Self::resolve_model_dir(handle)?;
        // T5：说话人 speaker embedding 模型（3d-speaker 等）可选；缺失时 SCD 降级为单说话人。
        let embedding_dir = Self::resolve_embedding_model_dir(handle);
        let scd_configured = embedding_dir.is_some();
        // Python/脚本路径统一走 model_paths::runtime_paths（已在内部处理
        // Windows Scripts/python.exe vs Unix bin/python3 的平台差异）。
        let runtime = crate::model_paths::runtime_paths(handle)?;
        let python = runtime.python;
        let script = runtime.script;

        if !python.is_file() {
            return Err(format!("找不到 sidecar Python：{python:?}（先创建 src-tauri/.venv 并 pip install sherpa-onnx）"));
        }
        if !script.is_file() {
            return Err(format!("找不到 sidecar 脚本：{script:?}"));
        }

        let mut cmd = Command::new(&python);
        cmd.arg(&script)
            .arg("--model-dir")
            .arg(&model_dir)
            .arg("--sample-rate")
            .arg("16000");
        if let Some(dir) = &embedding_dir {
            cmd.arg("--embedding-model-dir").arg(dir);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("启动 sherpa sidecar 失败: {e}"))?;

        let mut stdin = child
            .stdin
            .take()
            .ok_or("无法取得 sidecar stdin")?;
        // 协议首行：JSON 配置行
        writeln!(stdin, "{{\"type\":\"config\",\"sample_rate\":16000}}")
            .map_err(|e| format!("写 sidecar 配置行失败: {e}"))?;

        let finals = Arc::new(Mutex::new(VecDeque::new()));
        let partials = Arc::new(Mutex::new(VecDeque::new()));
        let status = Arc::new(AtomicU8::new(ST_STARTING));
        let last_error = Arc::new(Mutex::new(None));
        // T5：SCD 状态由 stdout 读线程独占持有（下方闭包内创建并 move 进线程，
        // 外部只读 scd_configured / scd_embedding_active，无需 struct 字段）。
        let scd_embedding = Arc::new(AtomicBool::new(false));

        // -- stdout 读取线程：NDJSON → 队列（final 经 SCD 决定 speaker_id） --
        let stdout = child.stdout.take().ok_or("无法取得 sidecar stdout")?;
        {
            let finals = Arc::clone(&finals);
            let partials = Arc::clone(&partials);
            let status = Arc::clone(&status);
            let last_error = Arc::clone(&last_error);
            let scd = Arc::new(Mutex::new(Scd::default()));
            let scd_embedding = Arc::clone(&scd_embedding);
            let handle = handle.clone();
            std::thread::spawn(move || {
                read_stdout(
                    stdout,
                    finals,
                    partials,
                    status,
                    last_error,
                    scd,
                    scd_embedding,
                    handle,
                )
            });
        }

        // -- 等待 sidecar 确认加载模型（started 事件）--
        // 若模型与当前 sherpa-onnx 不兼容，sidecar 会在初始化阶段崩溃（例如 X-ASR
        // 的 ONNX 缺 encoder_dims 元数据导致 segfault），此时既无 started 也无
        // error，stdout 直接 EOF。若不在此拦截，壳层会误报「本地 ASR 已启动」却
        // 一个字都识别不出来（静默无声故障，用户误以为麦克风没声音）。
        {
            let status = Arc::clone(&status);
            let last_error = Arc::clone(&last_error);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            loop {
                match status.load(Ordering::SeqCst) {
                    ST_STARTED => break,
                    ST_ERROR => {
                        let msg = last_error.lock().unwrap().clone().unwrap_or_else(|| "未知错误".to_string());
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(format!("sidecar 加载模型失败（{}）：{msg}", model_dir.display()));
                    }
                    ST_STOPPED => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(format!(
                            "sidecar 在加载 ASR 模型时退出（{}），模型可能不兼容当前 sherpa-onnx 版本",
                            model_dir.display()
                        ));
                    }
                    _ => {}
                }
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("sidecar 启动超时（{}），模型加载过慢或已卡死", model_dir.display()));
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }

        // -- 麦克风 → stdin 写线程 --
        let (tx, rx) = mpsc::channel::<Vec<f32>>();
        let (mic, src_rate) = crate::audio::start_mic_capture(tx)?;
        println!("[asr] 麦克风就绪（设备采样率 {src_rate}Hz → 16kHz），模型目录 {}", model_dir.display());
        println!(
            "[asr] SCD: {}",
            if scd_configured {
                format!("speaker embedding 模式（模型 {}）", embedding_dir.as_ref().unwrap().display())
            } else {
                "未配置 speaker embedding 模型，降级为单说话人（全部归说话人 1）".to_string()
            }
        );
        std::thread::spawn(move || {
            let mut stdin = stdin;
            for chunk in rx {
                let mut bytes = Vec::with_capacity(chunk.len() * 4);
                for s in chunk {
                    bytes.extend_from_slice(&s.to_le_bytes());
                }
                if stdin.write_all(&bytes).is_err() {
                    break; // sidecar 退出
                }
            }
        });

        Ok(Self {
            child,
            finals,
            partials,
            status,
            last_error,
            scd_embedding,
            scd_configured,
            mic,
        })
    }

    /// 解析模型目录：环境变量覆盖 → 按 `model-config.json` 所选模型解析（T16）。
    fn resolve_model_dir(handle: &AppHandle) -> Result<PathBuf, String> {
        if let Ok(dir) = std::env::var(MODEL_DIR_ENV) {
            let p = PathBuf::from(dir);
            return if p.is_dir() { Ok(p) } else { Err(format!("{MODEL_DIR_ENV} 指向的目录不存在: {p:?}")) };
        }
        crate::models::selected_asr_dir(handle)
    }

    /// 解析说话人 speaker embedding 模型目录（T5 SCD，可选）。
    ///
    /// 环境变量覆盖优先；否则按 `model-config.json` 所选 embedding 模型解析（T16）。
    /// 未配置/未下载返回 `None` —— 不传 `--embedding-model-dir`，sidecar 不加载
    /// speaker embedding 模型，SCD 降级为单说话人。
    fn resolve_embedding_model_dir(handle: &AppHandle) -> Option<PathBuf> {
        if let Ok(dir) = std::env::var(EMBEDDING_MODEL_DIR_ENV) {
            let p = PathBuf::from(dir);
            return if p.is_dir() { Some(p) } else { None };
        }
        crate::models::selected_embedding_dir(handle)
    }

    /// 是否向 sidecar 配置了 speaker embedding 模型（spawn 时决定；用于壳层日志）。
    pub fn scd_configured(&self) -> bool {
        self.scd_configured
    }

    /// sidecar 是否已确认加载 speaker embedding 模型（started 事件上报；真实 embedding 路径生效）。
    pub fn scd_embedding_active(&self) -> bool {
        self.scd_embedding.load(Ordering::SeqCst)
    }

    /// 取走并清空 partial 缓冲（壳层在事件循环中调用）。
    #[allow(dead_code)]
    pub fn take_partials(&self) -> Vec<String> {
        let mut q = self.partials.lock().unwrap();
        q.drain(..).collect()
    }

    /// 返回 partial 队列的共享句柄（引擎主循环轮询用）。
    pub fn partials_handle(&self) -> Arc<Mutex<VecDeque<String>>> {
        Arc::clone(&self.partials)
    }

    /// 当前 sidecar 状态（0 启动中 / 1 已启动 / 2 出错 / 3 已停止）。
    #[allow(dead_code)]
    pub fn state(&self) -> u8 {
        self.status.load(Ordering::SeqCst)
    }

    /// sidecar 报告的错误信息（若有）（T9 错误面板接入）。
    #[allow(dead_code)]
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().unwrap().clone()
    }

    /// 当前毫秒时间戳（Utterance.ts）。
    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

impl AsrPort for SherpaAsr {
    fn start(&mut self) {
        // 麦克风与 sidecar 已在 spawn 时启动；这里仅占位（trait 契约）。
    }

    fn stop(&mut self) {
        // 关闭 stdin（写线程随之退出）→ sidecar 优雅收尾 → kill
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.status.store(ST_STOPPED, Ordering::SeqCst);
    }

    fn next_utterance(&mut self) -> Option<Utterance> {
        let mut q = self.finals.lock().unwrap();
        q.pop_front().map(|mut utt| {
            if utt.ts == 0 {
                utt.ts = Self::now_ms();
            }
            utt
        })
    }
}

impl Drop for SherpaAsr {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// stdout 读取线程：逐行解析 NDJSON。
///
/// T5：`final` 事件若携带 `embedding` 字段（sidecar 加载了 speaker embedding
/// 模型），则经 [Scd] 余弦匹配决定 speaker_id / gender / is_new_speaker；
/// 否则降级为单说话人（speaker_id=1，保持 T4 行为）——注释即契约：未配置
/// embedding 模型时 SCD 降级，不强行用文本 hash 等伪向量制造假说话人。
/// SCD 追溯修正的共享状态（Tauri 托管，跨 stdout 读线程与引擎排空线程）。
///
/// - [`Self::seq_to_segment`]：`utt_seq → segment_id`，片段被追加进管线时记录；
/// - [`Self::corrections`]：待解析的修正队列 `(目标 utt_seq, 新说话人 id, 性别)`，
///   由 stdout 读线程在 SCD 返回 [`engine::SpeakerCorrection`] 时入队，引擎排空
///   线程解析成 [`EngineEvent::SpeakerCorrected`] 后出队。
#[derive(Default)]
pub(crate) struct CorrectionState {
    pub seq_to_segment: Mutex<std::collections::HashMap<u64, u64>>,
    pub corrections: Mutex<VecDeque<(u64, u32, Gender)>>,
}

fn read_stdout(
    stdout: std::process::ChildStdout,
    finals: Arc<Mutex<VecDeque<Utterance>>>,
    partials: Arc<Mutex<VecDeque<String>>>,
    status: Arc<AtomicU8>,
    last_error: Arc<Mutex<Option<String>>>,
    scd: Arc<Mutex<Scd>>,
    scd_embedding: Arc<AtomicBool>,
    handle: AppHandle,
) {
    use std::io::{BufRead, BufReader};
    // 每条 final 的唯一序号：SCD 追溯修正（SpeakerCorrection）引用它。
    let mut next_utt_seq: u64 = 0;
    let mut reader = BufReader::new(stdout);
    // 用 read_until（字节级）而非 lines()：lines()/read_line 在遇到非 UTF-8 字节时
    // 返回 Err 并 break 整个循环（Python sidecar 的中文输出若含无效 UTF-8 会导致
    // 读取线程提前退出，后续 partial/final 全部丢失）。read_until 按 \n 切分原始
    // 字节，UTF-8 解码失败时丢弃该行继续读取。
    let mut raw = Vec::new();
    loop {
        raw.clear();
        match reader.read_until(b'\n', &mut raw) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(_) => break,
        }
        let Ok(line) = std::str::from_utf8(&raw) else {
            eprintln!("[asr] 丢弃无法解码的 sidecar 行");
            continue;
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(obj) = serde_json::from_str::<serde_json::Value>(line) else {
            eprintln!("[asr] 无法解析 sidecar 输出: {line}");
            continue;
        };
        match obj.get("type").and_then(|v| v.as_str()) {
            Some("started") => {
                status.store(ST_STARTED, Ordering::SeqCst);
                // sidecar 确认是否加载了 speaker embedding 模型（scd_embedding_active() 依据）。
                scd_embedding.store(
                    obj.get("scd_embedding").and_then(|v| v.as_bool()).unwrap_or(false),
                    Ordering::SeqCst,
                );
                crate::pipeline::emit_scd_status(&handle, scd_embedding.load(Ordering::SeqCst));
                println!("[asr] sidecar 已启动: {}", obj.get("model").and_then(|v| v.as_str()).unwrap_or("?"));
            }
            Some("partial") => {
                if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        partials.lock().unwrap().push_back(text.to_string());
                    }
                }
            }
            Some("final") => {
                if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        let mut utt = Utterance {
                            speaker_id: 1,
                            gender: Gender::Unknown,
                            text: text.to_string(),
                            ts: 0, // Engine 端无 ts 时由 next_utterance 填充
                            is_new_speaker: None, // 降级路径：由 Engine 按已见说话人推导
                            utt_seq: None,
                        };
                        // 双轨降级：
                        // a) 真实 embedding —— sidecar 在 final 里附带该段音频的
                        //    speaker embedding + 有效语音时长，经 SCD 三段式判定
                        //    决定 speaker_id（时长门槛 / 时长自适应阈值 / 证据确认
                        //    + 追溯修正，见 engine::Scd）与性别；
                        // b) 无 embedding —— 保持单说话人（speaker_id=1）。
                        // sidecar 上报的有效语音时长（秒，裁静音后）；缺失按 0 处理，
                        // 触发时长门槛 → 沿用最近说话人（稳健降级）。
                        let speech_seconds = obj
                            .get("speech_duration")
                            .and_then(|v| v.as_f64())
                            .map(|v| v as f32)
                            .unwrap_or(0.0);
                        if let Some(arr) = obj.get("embedding").and_then(|v| v.as_array()) {
                            // NaN/Inf 防御：任一分量不是有限数字（null/字符串/NaN/Inf）
                            // → 整段视为无 embedding（走降级路径）。不静默丢弃分量：
                            // 部分 NaN 会污染点积 → 相似度被误判为 0 → 误新建说话人
                            // （引擎侧 [engine::Scd::is_empty_signal] 亦含 NaN 检测兜底）。
                            let emb: Vec<f32> = arr
                                .iter()
                                .map(|x| x.as_f64().map(|v| v as f32))
                                .collect::<Option<Vec<f32>>>()
                                .unwrap_or_default();
                            if !emb.is_empty() && emb.iter().all(|v| v.is_finite()) {
                                let mut scd = scd.lock().unwrap();
                                let d = scd.process_utterance(next_utt_seq, &text, &emb, speech_seconds, None);
                                utt.speaker_id = d.speaker_id;
                                utt.is_new_speaker = Some(d.is_new_speaker);
                                utt.gender = scd.template_gender(d.speaker_id).unwrap_or(Gender::Unknown);
                                // 证据确认产生的追溯修正：入共享队列，由引擎排空线程
                                // 解析成 SpeakerCorrected 事件。
                                if !d.corrections.is_empty() {
                                    let state = handle.state::<CorrectionState>();
                                    let gender = scd
                                        .template_gender(d.corrections[0].new_speaker_id)
                                        .unwrap_or(Gender::Unknown);
                                    let mut q = state.corrections.lock().unwrap();
                                    for corr in d.corrections {
                                        q.push_back((corr.utt_token, corr.new_speaker_id, gender));
                                    }
                                }
                            }
                        }
                        utt.utt_seq = Some(next_utt_seq);
                        next_utt_seq += 1;
                        finals.lock().unwrap().push_back(utt);
                    }
                }
            }
            Some("error") => {
                status.store(ST_ERROR, Ordering::SeqCst);
                let msg = obj.get("message").and_then(|v| v.as_str()).unwrap_or("?").to_string();
                eprintln!("[asr] sidecar 错误: {msg}");
                *last_error.lock().unwrap() = Some(msg);
            }
            Some("stopped") => {
                status.store(ST_STOPPED, Ordering::SeqCst);
            }
            other => eprintln!("[asr] 未知事件类型: {other:?}"),
        }
    }
    // stdout EOF → sidecar 已退出
    status.store(ST_STOPPED, Ordering::SeqCst);
}
