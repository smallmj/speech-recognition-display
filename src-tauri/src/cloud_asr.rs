//! 云端流式 ASR 客户端（T7，Deepgram 兼容协议）。
//!
//! 数据通路：
//! ```text
//! 麦克风（cpal，重采样 16kHz f32）
//!   -> tokio channel -> WebSocket binary（linear16 little-endian）
//!   -> Deepgram 兼容 /v1/listen 流式接口
//!   -> Results JSON：is_final=false -> partial；is_final=true -> final
//! ```
//!
//! 该模块刻意不把网络协议放进 engine；engine 只看到 [`engine::AsrPort`] 的
//! `next_utterance` 拉取契约。云端说话人仍归说话人 1；T5 的 SCD 依赖本地
//! speaker embedding，跨厂商云端协议不提供统一 embedding，因而不伪造结果。

use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use engine::{AsrPort, Gender, Utterance};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::watch;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::handshake::client::Request;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;

use crate::asr_config::AsrConfig;

const ST_STARTING: u8 = 0;
const ST_STARTED: u8 = 1;
const ST_ERROR: u8 = 2;
const ST_STOPPED: u8 = 3;

/// 云端 ASR 客户端。实现 [`AsrPort`]，partial/final 缓冲方式与本地 ASR 保持一致。
pub struct CloudAsr {
    finals: Arc<Mutex<VecDeque<Utterance>>>,
    partials: Arc<Mutex<VecDeque<String>>>,
    status: Arc<AtomicU8>,
    last_error: Arc<Mutex<Option<String>>>,
    shutdown: Arc<watch::Sender<bool>>,
    /// 持有时麦克风流存活；Drop 即停止采集。
    #[allow(dead_code)]
    mic: crate::audio::MicCapture,
}

impl CloudAsr {
    /// 启动麦克风 + WebSocket。握手结果同步返回给调用方，握手成功后识别在后台继续。
    pub fn spawn(config: AsrConfig) -> Result<Self, String> {
        if let Some(reason) = config.cloud_invalid_reason() {
            return Err(reason);
        }

        let (mic_tx, mic_rx) = mpsc::channel::<Vec<f32>>();
        let (mic, src_rate) = crate::audio::start_mic_capture(mic_tx)?;
        println!(
            "[asr] 云端 ASR 麦克风就绪（设备采样率 {src_rate}Hz -> 16kHz linear16），端点 {}",
            config.cloud_endpoint.trim()
        );

        // cpal 的同步回调 -> tokio WebSocket 任务。UnboundedSender::send 是同步方法，
        // 转发线程不需要进入异步运行时。
        let (audio_tx, mut audio_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<f32>>();
        std::thread::spawn(move || {
            for chunk in mic_rx {
                if audio_tx.send(chunk).is_err() {
                    break;
                }
            }
        });

        let finals = Arc::new(Mutex::new(VecDeque::new()));
        let partials = Arc::new(Mutex::new(VecDeque::new()));
        let status = Arc::new(AtomicU8::new(ST_STARTING));
        let last_error = Arc::new(Mutex::new(None));
        let (shutdown, mut shutdown_rx) = watch::channel(false);
        let shutdown = Arc::new(shutdown);
        let task_shutdown = Arc::clone(&shutdown);
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

        let task_finals = Arc::clone(&finals);
        let task_partials = Arc::clone(&partials);
        let task_status = Arc::clone(&status);
        let task_last_error = Arc::clone(&last_error);

        tauri::async_runtime::spawn(async move {
            let run = async {
                let request = build_request(&config)?;
                let (mut websocket, _response) = connect_async(request)
                    .await
                    .map_err(|e| format!("连接云端 ASR 失败: {e}"))?;

                let _ = ready_tx.send(Ok(()));
                task_status.store(ST_STARTED, Ordering::SeqCst);
                println!("[asr] 云端 ASR WebSocket 已连接");

                loop {
                    tokio::select! {
                        changed = shutdown_rx.changed() => {
                            if changed.is_err() || *shutdown_rx.borrow() {
                                let _ = websocket
                                    .send(Message::Close(Some(CloseFrame {
                                        code: CloseCode::Normal,
                                        reason: "client stopped".into(),
                                    })))
                                    .await;
                                break;
                            }
                        }
                        audio = audio_rx.recv() => {
                            let Some(chunk) = audio else {
                                let _ = websocket
                                    .send(Message::Close(Some(CloseFrame {
                                        code: CloseCode::Normal,
                                        reason: "microphone stopped".into(),
                                    })))
                                    .await;
                                break;
                            };
                            websocket
                                .send(Message::Binary(f32_chunk_to_linear16(&chunk)))
                                .await
                                .map_err(|e| format!("发送云端音频失败: {e}"))?;
                        }
                        message = websocket.next() => {
                            let Some(message) = message else { break };
                            let message = message
                                .map_err(|e| format!("读取云端 ASR 消息失败: {e}"))?;
                            match message {
                                Message::Text(text) => handle_deepgram_message(
                                    &text,
                                    &task_finals,
                                    &task_partials,
                                ),
                                Message::Close(_) => break,
                                Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => {}
                            }
                        }
                    }
                }
                Ok::<(), String>(())
            };

            match run.await {
                Ok(()) => task_status.store(ST_STOPPED, Ordering::SeqCst),
                Err(err) => {
                    let _ = ready_tx.send(Err(err.clone()));
                    task_status.store(ST_ERROR, Ordering::SeqCst);
                    *task_last_error.lock().unwrap() = Some(err);
                }
            }
        });

        // 握手有 10 秒上限，避免设置切换线程被错误端点无限挂住。
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => {}
            Ok(Err(err)) => return Err(err),
            Err(_) => {
                let _ = task_shutdown.send_replace(true);
                return Err("连接云端 ASR 超时（10s）".to_string());
            }
        }

        Ok(Self {
            finals,
            partials,
            status,
            last_error,
            shutdown,
            mic,
        })
    }

    /// 返回 partial 队列的共享句柄（引擎主循环轮询用）。
    pub fn partials_handle(&self) -> Arc<Mutex<VecDeque<String>>> {
        Arc::clone(&self.partials)
    }

    #[allow(dead_code)]
    pub fn state(&self) -> u8 {
        self.status.load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().unwrap().clone()
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

impl AsrPort for CloudAsr {
    fn start(&mut self) {
        // WebSocket 与麦克风在 spawn 时已启动；保留 trait 契约。
    }

    fn stop(&mut self) {
        let _ = self.shutdown.send_replace(true);
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

impl Drop for CloudAsr {
    fn drop(&mut self) {
        let _ = self.shutdown.send_replace(true);
    }
}

/// 构造带鉴权头的 WebSocket 握手请求。Deepgram 使用 `Authorization: Token`。
fn build_request(config: &AsrConfig) -> Result<Request, String> {
    Request::builder()
        .uri(websocket_url(config))
        .header(
            "Authorization",
            format!("Token {}", config.cloud_api_key.trim()),
        )
        .header("User-Agent", "speech-caption-display/0.1")
        .body(())
        .map_err(|e| format!("构造云端 ASR 请求失败: {e}"))
}

/// 构造 Deepgram 流式 URL：保留用户端点已有 query，再附加客户端协议参数。
fn websocket_url(config: &AsrConfig) -> String {
    let endpoint = config.cloud_endpoint.trim();
    let (base, hash) = match endpoint.split_once('#') {
        Some((base, hash)) => (base, hash),
        None => (endpoint, ""),
    };
    let separator = if base.contains('?') { '&' } else { '?' };
    let query = [
        ("encoding", "linear16"),
        ("sample_rate", "16000"),
        ("channels", "1"),
        ("interim_results", "true"),
        ("endpointing", "100"),
        ("model", config.cloud_model.trim()),
        ("language", config.cloud_language.trim()),
    ];
    let params: Vec<String> = query
        .into_iter()
        .filter(|(key, _)| !endpoint_has_query_param(base, key))
        .map(|(key, value)| format!("{}={}", encode_query(key), encode_query(value)))
        .collect();

    if hash.is_empty() {
        format!("{base}{separator}{}", params.join("&"))
    } else {
        format!("{base}{separator}{}#{hash}", params.join("&"))
    }
}

/// 判断用户端点是否已显式提供某个 query 参数；已提供则尊重用户值，不重复追加。
fn endpoint_has_query_param(base: &str, key: &str) -> bool {
    base.split_once('?')
        .map(|(_, query)| {
            query.split('&').any(|pair| {
                pair.split('=')
                    .next()
                    .map(|existing| existing == key)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// 简单 percent-encoding：模型名与语言代码允许出现 Unicode、`/`、`+` 等字符。
fn encode_query(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// f32 [-1, 1] -> little-endian i16，供 Deepgram linear16 二进制帧使用。
fn f32_chunk_to_linear16(chunk: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(chunk.len() * 2);
    for sample in chunk {
        let clamped = if sample.is_finite() {
            sample.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let value = (clamped * i16::MAX as f32).round() as i16;
        let _ = bytes.write_all(&value.to_le_bytes());
    }
    bytes
}

/// 解析 Deepgram Results JSON 并写入对应缓冲。
fn handle_deepgram_message(
    raw: &str,
    finals: &Mutex<VecDeque<Utterance>>,
    partials: &Mutex<VecDeque<String>>,
) {
    let DeepgramResult {
        transcript,
        is_final,
    } = match parse_deepgram_result(raw) {
        Some(result) if !result.transcript.trim().is_empty() => result,
        _ => return,
    };

    if is_final {
        finals.lock().unwrap().push_back(Utterance {
            speaker_id: 1,
            gender: Gender::Unknown,
            text: transcript.trim().to_string(),
            ts: 0,
            is_new_speaker: None,
        });
    } else {
        partials
            .lock()
            .unwrap()
            .push_back(transcript.trim().to_string());
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeepgramResult {
    transcript: String,
    is_final: bool,
}

/// 只提取协议必需字段，忽略 metadata / words 等无关负载。
fn parse_deepgram_result(raw: &str) -> Option<DeepgramResult> {
    let obj: serde_json::Value = serde_json::from_str(raw).ok()?;
    let transcript = obj
        .get("channel")?
        .get("alternatives")?
        .get(0)?
        .get("transcript")?
        .as_str()?
        .to_string();
    let is_final = obj
        .get("is_final")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Some(DeepgramResult {
        transcript,
        is_final,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_deepgram_streaming_url_with_existing_query() {
        let config = AsrConfig {
            cloud_endpoint: "wss://example.com/listen?custom=1".to_string(),
            cloud_api_key: "secret".to_string(),
            cloud_model: "nova-3".to_string(),
            cloud_language: "multi".to_string(),
            ..AsrConfig::default()
        };
        let url = websocket_url(&config);
        assert!(url.starts_with("wss://example.com/listen?custom=1&"));
        assert!(url.contains("encoding=linear16"));
        assert!(url.contains("sample_rate=16000"));
        assert!(url.contains("channels=1"));
        assert!(url.contains("interim_results=true"));
        assert!(url.contains("endpointing=100"));
        assert!(url.contains("model=nova-3"));
        assert!(url.contains("language=multi"));
    }

    #[test]
    fn preserves_explicit_endpoint_query_parameters() {
        let config = AsrConfig {
            cloud_endpoint: "wss://example.com/listen?language=zh&model=custom".to_string(),
            cloud_api_key: "secret".to_string(),
            cloud_model: "nova-3".to_string(),
            cloud_language: "multi".to_string(),
            ..AsrConfig::default()
        };
        let url = websocket_url(&config);
        assert!(url.contains("language=zh&"));
        assert!(url.contains("model=custom&"));
        assert!(!url.contains("language=multi"));
        assert!(!url.contains("model=nova-3"));
        assert!(url.contains("encoding=linear16"));
    }

    #[test]
    fn builds_authorized_websocket_request() {
        let config = AsrConfig {
            cloud_api_key: " secret ".to_string(),
            ..AsrConfig::default()
        };
        let request = build_request(&config).unwrap();
        assert!(request
            .uri()
            .to_string()
            .starts_with("wss://api.deepgram.com/v1/listen?"));
        assert_eq!(
            request.headers().get("Authorization").unwrap(),
            "Token secret"
        );
    }

    #[test]
    fn encodes_unicode_language_and_reserved_characters() {
        assert_eq!(encode_query("zh-CN"), "zh-CN");
        assert_eq!(encode_query("中文"), "%E4%B8%AD%E6%96%87");
        assert_eq!(encode_query("a b+c"), "a%20b%2Bc");
    }

    #[test]
    fn parses_interim_and_final_deepgram_results() {
        let interim = parse_deepgram_result(
            r#"{"type":"Results","is_final":false,"channel":{"alternatives":[{"transcript":"你好"}]}}"#,
        )
        .unwrap();
        assert_eq!(
            interim,
            DeepgramResult {
                transcript: "你好".to_string(),
                is_final: false
            }
        );

        let final_result = parse_deepgram_result(
            r#"{"type":"Results","is_final":true,"channel":{"alternatives":[{"transcript":"Hello, world."}]}}"#,
        )
        .unwrap();
        assert!(final_result.is_final);
        assert_eq!(final_result.transcript, "Hello, world.");
    }

    #[test]
    fn deepgram_message_goes_to_partial_or_final_queue() {
        let finals = Mutex::new(VecDeque::new());
        let partials = Mutex::new(VecDeque::new());
        handle_deepgram_message(
            r#"{"type":"Results","is_final":false,"channel":{"alternatives":[{"transcript":"hello"}]}}"#,
            &finals,
            &partials,
        );
        handle_deepgram_message(
            r#"{"type":"Results","is_final":true,"channel":{"alternatives":[{"transcript":"你好。"}]}}"#,
            &finals,
            &partials,
        );
        assert_eq!(
            partials.lock().unwrap().pop_front().as_deref(),
            Some("hello")
        );
        let utt = finals.lock().unwrap().pop_front().unwrap();
        assert_eq!(utt.text, "你好。");
        assert_eq!(utt.speaker_id, 1);
    }

    #[test]
    fn converts_float_samples_to_little_endian_i16() {
        assert_eq!(
            f32_chunk_to_linear16(&[0.0, 1.0, -1.0]),
            vec![0, 0, 255, 127, 1, 128]
        );
    }
}
