//! 真实 LLM 客户端：OpenAI 兼容接口（Base URL + API Key + 模型名）。
//!
//! T9 把模拟 LLM 换成真实接口，落在 Tauri 壳（engine 刻意不依赖网络/异步
//! 运行时，只扩展了事件契约）。本模块承担三件事：
//!
//! - **配置持久化**：[LlmConfig] 以明文 JSON 存于 app config 目录
//!   （`llm-config.json`），`load_llm_config` / `save_llm_config` 两个
//!   `#[tauri::command]` 供前端保存/读取。API Key 明文存储是 MVP 的已知
//!   取舍（规格未要求加密）。
//! - **OpenAI 兼容 SSE 客户端**：[OpenAiLlmClient] 实现
//!   [engine::LlmPort] 的 `cleanup_streaming`（阻塞式 POST
//!   `{base_url}/chat/completions`，`Authorization: Bearer`，逐行解析
//!   `data: {...}`，取 `choices[0].delta.content` 增量回调）。
//! - **退避重试**：网络错误 / 非 2xx / SSE 解析错误 → 等比退避（500ms/1s/2s）
//!   重试 3 次（共 4 次尝试），全部失败返回 [LlmError]，由驱动线程经
//!   `fail_pending` 置 `Failed`，前端回退展示原文（对齐 ADR-0003「失败退避
//!   重试 3 次后放弃」）。
//! - **输入窗口**：ADR-0003「单次输入 ≤500 字」——超长原文按标点切分只送
//!   首个完整窗口；滚动窗口 ≤2000 token 的约束在 [MAX_INPUT_CHARS] 注释说明。
//!
//! **整理人设**：[DEFAULT_PERSONA] 内置整理人设（参考 TypeFlux 意图整理人设：
//! 去口语化/纠错/补标点/不改意）；`LlmConfig.persona` 为空时自动回退内置默认。

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// 配置文件文件名（存于 app config 目录）。
const CONFIG_FILE: &str = "llm-config.json";

/// 内置整理人设（系统提示词）：口语化原文 → 通顺书面语。
///
/// 风格对齐 TypeFlux 意图整理人设：去口语化、纠错、补标点、不改变原意、
/// 不添加原话没有的信息、直接输出整理结果。
///
/// **权威源在本文件**；前端 `src/components/LlmConfigPanel.tsx` 的
/// `DEFAULT_PERSONA` 与此保持一致（「恢复内置人设」按钮用），改动需两处同步。
pub const DEFAULT_PERSONA: &str = "你是实时字幕整理助手：把用户提供的口语化原文整理成通顺的书面语，去口语化、纠正错别字、补充标点，不改变原意，不添加原话没有的信息。直接输出整理结果，不要任何解释或前缀。";

/// OpenAI 兼容接口配置（serde camelCase，与前端契约对齐；字段缺省用 [Default]）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LlmConfig {
    /// 接口 Base URL（如 `https://api.openai.com/v1`）。请求路径为 `{base_url}/chat/completions`。
    pub base_url: String,
    /// API Key（明文存本地配置文件，MVP 可接受）。
    pub api_key: String,
    /// 模型名（如 `gpt-4o-mini`）。
    pub model: String,
    /// 整理人设：`None` / 空白 → 使用内置默认 [DEFAULT_PERSONA]。
    pub persona: Option<String>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: String::new(),
            model: "gpt-4o-mini".to_string(),
            persona: None,
        }
    }
}

impl LlmConfig {
    /// 实际生效的整理人设：显式配置为空时回退内置默认。
    pub fn effective_persona(&self) -> &str {
        match self.persona.as_deref() {
            Some(p) if !p.trim().is_empty() => p,
            _ => DEFAULT_PERSONA,
        }
    }

    /// chat/completions 完整 URL（base_url 末尾斜杠容错）。
    pub fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }
}

/// 配置文件路径（app config 目录下）。
fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|dir| dir.join(CONFIG_FILE))
        .map_err(|e| format!("无法获取 app config 目录: {e}"))
}

/// 读取配置；文件不存在 / 解析失败 → 默认配置（驱动线程容错：缺配置时
/// 请求会失败并走「重试 3 次 → 回退原文」的既定路径，不 panic）。
pub(crate) fn read_config(app: &AppHandle) -> LlmConfig {
    let Ok(path) = config_path(app) else {
        return LlmConfig::default();
    };
    let Ok(raw) = fs::read_to_string(&path) else {
        return LlmConfig::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// 前端加载配置命令：返回当前保存的配置（未保存过 → 默认配置）。
#[tauri::command]
pub fn load_llm_config(app: AppHandle) -> Result<LlmConfig, String> {
    Ok(read_config(&app))
}

/// 前端保存配置命令：明文 JSON 写入 app config 目录。
#[tauri::command]
pub fn save_llm_config(app: AppHandle, config: LlmConfig) -> Result<(), String> {
    let path = config_path(&app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建配置目录失败: {e}"))?;
    }
    let json = serde_json::to_string_pretty(&config).map_err(|e| format!("序列化配置失败: {e}"))?;
    fs::write(&path, json).map_err(|e| format!("写入配置失败: {e}"))
}

/// LLM 调用错误（可重试的失败类型）。
#[derive(Debug)]
pub enum LlmError {
    /// 网络/传输错误（连接失败、超时、TLS 等）。
    Transport(String),
    /// HTTP 非 2xx 状态码。
    Http { status: u16, body: String },
    /// SSE 流解析失败 / 空响应 / 无整理内容。
    Stream(String),
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::Transport(e) => write!(f, "网络错误: {e}"),
            LlmError::Http { status, body } => write!(f, "HTTP {status}: {body}"),
            LlmError::Stream(e) => write!(f, "流错误: {e}"),
        }
    }
}

/// OpenAI 兼容客户端：配置 + 流式整理 + 退避重试。实现 [engine::LlmPort]。
pub struct OpenAiLlmClient {
    config: LlmConfig,
    agent: ureq::Agent,
}

/// 最大尝试次数（1 次初始请求 + 3 次重试，对齐 ADR-0003「重试 3 次后放弃」）。
const MAX_ATTEMPTS: u32 = 4;
/// 退避间隔（等比：第 i 次重试前等待 `BACKOFF_MS[i]` 毫秒；共 3 次重试）。
const BACKOFF_MS: [u64; 3] = [500, 1000, 2000];

/// ADR-0003「单次输入 ≤500 字」：超长原文按标点切分只送首个完整窗口。
/// 滚动窗口 ≤2000 token 的约束由本常量 + 每条 final 片段天然较短共同满足
/// （估算 ~1 token/汉字，500 字 ≈ 500-700 token，留足滚动上文余量）。
const MAX_INPUT_CHARS: usize = 500;

impl OpenAiLlmClient {
    pub fn new(config: LlmConfig) -> Self {
        Self {
            config,
            // 整体超时 120s，读取（流）超时 60s，防挂死。
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(120))
                .timeout_read(Duration::from_secs(60))
                .build(),
        }
    }

    /// 把原文截到首个完整窗口：≤[MAX_INPUT_CHARS] 直接返回；超长则在窗口内
    /// 最后一个句末标点（。！？…）处切断（保留标点），找不到标点则硬截断。
    /// 不改变原意——只发送窗口内的内容，窗口外内容由后续片段另行整理。
    fn clip_input_window(raw: &str) -> String {
        if raw.chars().count() <= MAX_INPUT_CHARS {
            return raw.to_string();
        }
        let chars: Vec<char> = raw.chars().collect();
        let limit = MAX_INPUT_CHARS.min(chars.len());
        // 找窗口内最后一个句末标点位置（不含窗口边界）
        let cut = (0..limit)
            .rev()
            .find(|&i| matches!(chars[i], '。' | '！' | '？' | '…'))
            .map(|i| i + 1) // 保留标点本身
            .unwrap_or(limit);
        chars[..cut].iter().collect()
    }

    /// 单次流式请求：POST chat/completions，逐行解析 SSE，回调增量。
    fn stream_once(
        &self,
        raw: &str,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<String, LlmError> {
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": [
                { "role": "system", "content": self.config.effective_persona() },
                { "role": "user", "content": raw },
            ],
            "stream": true,
        });

        let resp = self
            .agent
            .post(&self.config.chat_url())
            .set("Authorization", &format!("Bearer {}", self.config.api_key))
            .set("Content-Type", "application/json")
            .send_json(&body)
            .map_err(|e| LlmError::Transport(e.to_string()))?;

        let status = resp.status();
        if !(200..300).contains(&status) {
            let body_text = resp.into_string().unwrap_or_default();
            return Err(LlmError::Http { status, body: body_text });
        }

        // 按 Content-Type 分流：SSE（text/event-stream）逐行流式解析；
        // 其余（某些兼容服务把结果整体返回）走整段解析兜底。
        let is_sse = resp
            .header("Content-Type")
            .is_some_and(|ct| ct.contains("text/event-stream"));
        if !is_sse {
            return self.parse_non_stream(resp);
        }

        // SSE：`data: {...}` 每行一个 JSON，取 choices[0].delta.content。
        let reader = BufReader::new(resp.into_reader());
        let mut accumulated = String::new();
        for line in reader.lines() {
            let line = line.map_err(|e| LlmError::Stream(format!("读取流失败: {e}")))?;
            let line = line.trim_end();
            if line.is_empty() {
                continue;
            }
            let Some(payload) = line.trim_start().strip_prefix("data:") else {
                continue; // `event:` / `:` 注释等行，忽略
            };
            let payload = payload.trim();
            if payload == "[DONE]" {
                break;
            }
            let chunk: serde_json::Value = serde_json::from_str(payload)
                .map_err(|e| LlmError::Stream(format!("SSE JSON 解析失败: {e}（行: {payload}）")))?;
            // 空 choices / 无 delta 的保活帧直接跳过
            if let Some(content) = chunk
                .pointer("/choices/0/delta/content")
                .and_then(|v| v.as_str())
            {
                accumulated.push_str(content);
                on_delta(&accumulated);
            }
        }
        if accumulated.is_empty() {
            return Err(LlmError::Stream("SSE 流结束但未收到任何内容".to_string()));
        }
        Ok(accumulated)
    }

    /// 非 SSE 响应兜底：整段 JSON 取 `choices[0].message.content`（或 delta.content）。
    fn parse_non_stream(&self, resp: ureq::Response) -> Result<String, LlmError> {
        let full = resp
            .into_string()
            .map_err(|e| LlmError::Stream(format!("读取响应失败: {e}")))?;
        let v: serde_json::Value = serde_json::from_str(&full)
            .map_err(|e| LlmError::Stream(format!("响应 JSON 解析失败: {e}")))?;
        let text = v
            .pointer("/choices/0/message/content")
            .and_then(|c| c.as_str())
            .or_else(|| v.pointer("/choices/0/delta/content").and_then(|c| c.as_str()))
            .ok_or_else(|| LlmError::Stream("响应中未找到整理内容".to_string()))?;
        Ok(text.to_string())
    }
}

/// 实现 [engine::LlmPort]：驱动侧经 trait 对象调用（`Box<dyn LlmPort>`），
/// engine 测试缝可覆盖流式路径；失败返回错误信息由驱动走 `fail_pending`。
impl engine::LlmPort for OpenAiLlmClient {
    fn cleanup(&self, text: &str) -> String {
        // 同步兜底：不做真实请求（网络调用只在流式路径发生）；
        // 直接返回原文保证不丢内容（调用方不应走本方法）。
        text.to_string()
    }

    fn cleanup_streaming(
        &self,
        raw: &str,
        on_partial: &mut dyn FnMut(&str),
    ) -> Result<String, String> {
        // ADR-0003 输入窗口：超 500 字只送首个完整窗口。
        let window = Self::clip_input_window(raw);
        let mut last_err: Option<LlmError> = None;
        for attempt in 0..MAX_ATTEMPTS {
            if attempt > 0 {
                let backoff = BACKOFF_MS[(attempt - 1) as usize];
                println!("[llm] 重试 {attempt}/{}（{backoff}ms 后）", MAX_ATTEMPTS - 1);
                std::thread::sleep(Duration::from_millis(backoff));
            }
            match self.stream_once(&window, on_partial) {
                Ok(text) => return Ok(text),
                Err(e) => {
                    println!("[llm] 第 {} 次请求失败: {e}", attempt + 1);
                    last_err = Some(e);
                }
            }
        }
        Err(last_err
            .unwrap_or_else(|| LlmError::Stream("未知错误".to_string()))
            .to_string())
    }

    fn summarize(&self, _chunks: &[String]) -> String {
        // T10 起接入真实纪要（本文件在 T9 分支暂无调用方）。
        String::new()
    }
}
