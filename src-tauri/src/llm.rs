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

/// 内置纪要人设（系统提示词）：把分批的会议原文汇总为结构化会议纪要。
///
/// 对齐规格「纪要编排」：输出结构化纪要（要点/行动项/待办）。同一人设用于
/// 两个阶段：逐批生成部分纪要（每个时间窗一份），再汇总为最终纪要。
pub const MINUTES_PERSONA: &str = "你是会议纪要助手：把用户提供的会议原文内容整理成结构化会议纪要。请严格按以下分节输出，每节用【】标题并分条列出：【要点】会议核心结论与关键信息；【行动项】需要执行的具体事项（注明负责人与时间）；【待办】尚未明确或需后续跟进的事项。只输出纪要正文，不要任何解释或前缀。";

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

    /// models 完整 URL（OpenAI 兼容接口）。
    pub fn models_url(base_url: &str) -> String {
        format!("{}/models", base_url.trim_end_matches('/'))
    }
}

/// OpenAI 兼容 `/models` 返回的可选模型。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmModelSummary {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_by: Option<String>,
}

/// 前端获取模型列表命令：使用设置面板当前填写的 Base URL / API Key，
/// 调 OpenAI 兼容 `GET /models`，返回可选中模型。该命令不保存配置。
#[tauri::command]
pub fn list_llm_models(base_url: String, api_key: String) -> Result<Vec<LlmModelSummary>, String> {
    let trimmed_base = base_url.trim();
    let trimmed_key = api_key.trim();
    if trimmed_base.is_empty() {
        return Err("请先填写 Base URL".to_string());
    }
    if trimmed_key.is_empty() {
        return Err("请先填写 API Key".to_string());
    }

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .build();
    let resp = agent
        .get(&LlmConfig::models_url(trimmed_base))
        .set("Authorization", &format!("Bearer {trimmed_key}"))
        .call()
        .map_err(|e| format!("获取模型列表失败: {e}"))?;

    let status = resp.status();
    if !(200..300).contains(&status) {
        let body = resp.into_string().unwrap_or_default();
        return Err(format!("获取模型列表失败: HTTP {status}: {body}"));
    }

    let body = resp
        .into_string()
        .map_err(|e| format!("读取模型列表失败: {e}"))?;
    parse_model_list_response(&body).map_err(|e| format!("解析模型列表失败: {e}"))
}

/// 解析 OpenAI 兼容 `/models` 响应：`data[].id`。
fn parse_model_list_response(raw: &str) -> Result<Vec<LlmModelSummary>, String> {
    #[derive(Deserialize)]
    struct ApiModel {
        id: String,
        owned_by: Option<String>,
    }

    #[derive(Deserialize)]
    struct ModelListResponse {
        data: Vec<ApiModel>,
    }

    serde_json::from_str::<ModelListResponse>(raw)
        .map(|resp| {
            resp.data
                .into_iter()
                .map(|model| LlmModelSummary {
                    id: model.id,
                    owned_by: model.owned_by,
                })
                .collect()
        })
        .map_err(|e| e.to_string())
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

/// ADR-0003「单次输入 ≤500 字」：超长原文切成多个 ≤500 字窗口**逐个整理并
/// 拼接**（[OpenAiLlmClient::input_windows]），不丢内容也不改意。每个窗口
/// ≤500 字 ≈ ≤700 token，故单请求天然低于「滚动窗口 ≤2000 token」上限
/// （该上限主要约束 T10 纪要的分批汇总，逐段整理路径无需维护滚动上下文）。
const MAX_INPUT_CHARS: usize = 500;

/// 大小写不敏感地查找标签位置（标签均为 ASCII，字节窗口不会切断 UTF-8 字符）。
fn find_tag_from(haystack: &str, tag: &str, from: usize) -> Option<usize> {
    let bytes = haystack.as_bytes();
    let tag_bytes = tag.as_bytes();
    if from > bytes.len() || bytes.len() < tag_bytes.len() {
        return None;
    }
    (from..=bytes.len() - tag_bytes.len())
        .find(|&start| bytes[start..start + tag_bytes.len()].eq_ignore_ascii_case(tag_bytes))
}

/// 隐藏模型输出中的思考内容：
/// - 完整 `<think>...</think>` 保留标签外的正文；
/// - 流式输出尚未闭合时，只显示标签前已有正文；
/// - 流式输出刚开始出现 `<thin` 这类未完整标签时暂缓显示，避免闪出半截标签。
fn strip_reasoning_tags(input: &str) -> String {
    const OPEN_TAG: &str = "<think>";
    const CLOSE_TAG: &str = "</think>";
    let mut output = String::new();
    let mut cursor = 0usize;
    let mut inside_reasoning = false;

    while cursor < input.len() {
        if inside_reasoning {
            match find_tag_from(input, CLOSE_TAG, cursor) {
                Some(end) => {
                    cursor = end + CLOSE_TAG.len();
                    inside_reasoning = false;
                }
                None => break,
            }
        } else {
            let open = find_tag_from(input, OPEN_TAG, cursor);
            let close = find_tag_from(input, CLOSE_TAG, cursor);
            match (open, close) {
                (Some(start), Some(end)) if end < start => {
                    output.push_str(&input[cursor..end]);
                    cursor = end + CLOSE_TAG.len();
                }
                (Some(start), _) => {
                    output.push_str(&input[cursor..start]);
                    cursor = start + OPEN_TAG.len();
                    inside_reasoning = true;
                }
                (None, Some(end)) => {
                    output.push_str(&input[cursor..end]);
                    cursor = end + CLOSE_TAG.len();
                }
                (None, None) => {
                    output.push_str(&input[cursor..]);
                    break;
                }
            }
        }
    }

    // 流式期间 `<thi` 可能是尚未到齐的 `<think>` 前缀；先隐藏，下一帧再判断。
    if !inside_reasoning {
        for len in (1..OPEN_TAG.len()).rev() {
            let output_bytes = output.as_bytes();
            if output_bytes.len() >= len
                && output_bytes[output_bytes.len() - len..]
                    .eq_ignore_ascii_case(&OPEN_TAG.as_bytes()[..len])
            {
                let boundary = output.len() - len;
                output.truncate(boundary);
                break;
            }
        }
    }

    output.trim_start().to_string()
}

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

    /// 把原文切成若干 ≤[MAX_INPUT_CHARS] 的窗口，**覆盖全文（不丢内容）**。
    /// 优先在窗口内最后一个句末标点（。！？…）处切分（保留标点），找不到标点
    /// 则硬截断到窗口上限；下一窗口从切断处继续，直至覆盖全部字符。
    fn input_windows(raw: &str) -> Vec<String> {
        let chars: Vec<char> = raw.chars().collect();
        let mut windows = Vec::new();
        let mut start = 0usize;
        while start < chars.len() {
            let end = (start + MAX_INPUT_CHARS).min(chars.len());
            // 找窗口内最后一个句末标点（不含 end 边界），切在标点之后；否则硬截到 end。
            let cut = (start..end)
                .rev()
                .find(|&i| matches!(chars[i], '。' | '！' | '？' | '…'))
                .map(|i| i + 1)
                .unwrap_or(end);
            // 防御：若标点切分导致窗口为空（cut <= start），退回 hard 截断。
            let cut = if cut <= start { end } else { cut };
            windows.push(chars[start..cut].iter().collect());
            start = cut;
        }
        windows
    }

    /// 单次流式请求（整理人设）：POST chat/completions，逐行解析 SSE，回调增量。
    fn stream_once(&self, raw: &str, on_delta: &mut dyn FnMut(&str)) -> Result<String, LlmError> {
        self.chat_once(self.config.effective_persona(), raw, on_delta)
    }

    /// 单次 chat 请求：POST chat/completions，逐行解析 SSE，回调增量。
    /// `persona` 允许整理（[DEFAULT_PERSONA]）与纪要（[MINUTES_PERSONA]）共用同一套
    /// 传输/解析/重试逻辑（T10 复用 T9 的 SSE 客户端）。
    fn chat_once(
        &self,
        persona: &str,
        raw: &str,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<String, LlmError> {
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": [
                { "role": "system", "content": persona },
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
            return Err(LlmError::Http {
                status,
                body: body_text,
            });
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
            let chunk: serde_json::Value = serde_json::from_str(payload).map_err(|e| {
                LlmError::Stream(format!("SSE JSON 解析失败: {e}（行: {payload}）"))
            })?;
            // 空 choices / 无 delta 的保活帧直接跳过
            if let Some(content) = chunk
                .pointer("/choices/0/delta/content")
                .and_then(|v| v.as_str())
            {
                accumulated.push_str(content);
                let visible = strip_reasoning_tags(&accumulated);
                on_delta(&visible);
            }
        }
        if accumulated.is_empty() {
            return Err(LlmError::Stream("SSE 流结束但未收到任何内容".to_string()));
        }
        let visible = strip_reasoning_tags(&accumulated);
        if visible.trim().is_empty() {
            return Err(LlmError::Stream(
                "模型只返回了思考内容，没有整理结果".to_string(),
            ));
        }
        Ok(visible)
    }

    /// 带退避重试地执行一次 chat 请求：失败按等比退避（500ms/1s/2s）重试，
    /// 最多 [MAX_ATTEMPTS] 次尝试（1 次初始 + 3 次重试）后放弃。
    fn run_with_retries(
        &self,
        mut attempt: impl FnMut(&Self) -> Result<String, LlmError>,
    ) -> Result<String, LlmError> {
        let mut last_err: Option<LlmError> = None;
        for attempt_no in 0..MAX_ATTEMPTS {
            if attempt_no > 0 {
                let backoff = BACKOFF_MS[(attempt_no - 1) as usize];
                println!(
                    "[llm] 第 {attempt_no} 次重试（{backoff}ms 后，共 {MAX_ATTEMPTS} 次尝试）"
                );
                std::thread::sleep(Duration::from_millis(backoff));
            }
            match attempt(self) {
                Ok(text) => return Ok(text),
                Err(e) => {
                    println!("[llm] 第 {} 次请求失败: {e}", attempt_no + 1);
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| LlmError::Stream("未知错误".to_string())))
    }

    /// 纪要 prompt：各批文本（或各批部分纪要）按【第 N 段】标记拼接，供
    /// [MINUTES_PERSONA] 汇总；顺序即时间窗顺序。
    fn minutes_prompt(chunks: &[String]) -> String {
        let mut user = String::new();
        for (i, chunk) in chunks.iter().enumerate() {
            if i > 0 {
                user.push('\n');
            }
            user.push_str(&format!("【第 {} 段】\n{}", i + 1, chunk));
        }
        user
    }

    /// 生成会议纪要的 Result 形态（T10 壳层用于「失败回退该批原文」兜底）。
    pub(crate) fn summarize_result(&self, chunks: &[String]) -> Result<String, String> {
        self.summarize_with_retries(chunks)
            .map_err(|e| e.to_string())
    }

    /// 生成会议纪要（带退避重试）：整段返回最终纪要，失败返回错误信息字符串。
    fn summarize_with_retries(&self, chunks: &[String]) -> Result<String, LlmError> {
        if chunks.is_empty() {
            return Ok("（无内容）".to_string());
        }
        let user = Self::minutes_prompt(chunks);
        let mut noop = |_: &str| {};
        self.run_with_retries(move |client| client.chat_once(MINUTES_PERSONA, &user, &mut noop))
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
            .or_else(|| {
                v.pointer("/choices/0/delta/content")
                    .and_then(|c| c.as_str())
            })
            .ok_or_else(|| LlmError::Stream("响应中未找到整理内容".to_string()))?;
        let visible = strip_reasoning_tags(text);
        if visible.trim().is_empty() {
            return Err(LlmError::Stream(
                "模型只返回了思考内容，没有整理结果".to_string(),
            ));
        }
        Ok(visible)
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
        // ADR-0003「单次输入 ≤500 字」：把原文切成全覆盖窗口，逐窗口整理后拼接，
        // 不丢内容也不改意。每个窗口 ≤500 字（≈≤700 token），故单请求远低于
        // 「滚动窗口 ≤2000 token」上限（该约束主要约束 T10 纪要的分批汇总，
        // 逐段整理路径天然满足、无滚动上下文需要维护）。
        let windows = Self::input_windows(raw);
        let mut finished_prefix = String::new();

        for window in windows {
            let mut last_err: Option<LlmError> = None;
            let mut cleaned: Option<String> = None;
            for attempt in 0..MAX_ATTEMPTS {
                if attempt > 0 {
                    let backoff = BACKOFF_MS[(attempt - 1) as usize];
                    println!(
                        "[llm] 重试 {attempt}/{}（{backoff}ms 后）",
                        MAX_ATTEMPTS - 1
                    );
                    std::thread::sleep(Duration::from_millis(backoff));
                }
                // partial 携带「已完成窗口拼接前缀 + 当前窗口增量」，保证整段
                // 视角的整理文本随时间单调（重试时当前窗口重置，前缀不变，
                // 前端用长度单调守卫抑制回退抖动）。
                let prefix = finished_prefix.clone();
                match self.stream_once(&window, &mut |partial| {
                    let mut full = prefix.clone();
                    full.push_str(partial);
                    on_partial(&full);
                }) {
                    Ok(text) => {
                        cleaned = Some(text);
                        break;
                    }
                    Err(e) => {
                        println!("[llm] 第 {} 次请求失败: {e}", attempt + 1);
                        last_err = Some(e);
                    }
                }
            }
            match cleaned {
                Some(text) => finished_prefix.push_str(&text),
                None => {
                    return Err(last_err
                        .unwrap_or_else(|| LlmError::Stream("未知错误".to_string()))
                        .to_string());
                }
            }
        }

        Ok(finished_prefix)
    }

    fn summarize(&self, chunks: &[String]) -> String {
        match self.summarize_result(chunks) {
            Ok(minutes) => minutes,
            Err(err) => {
                eprintln!("[llm] 纪要生成失败: {err}");
                format!("纪要生成失败（重试 3 次后放弃）：{err}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::LlmPort;

    /// T10：纪要 prompt 按【第 N 段】标记拼接各批（部分纪要），顺序即时间窗顺序。
    #[test]
    fn minutes_prompt_joins_chunks_with_batch_markers() {
        let prompt = OpenAiLlmClient::minutes_prompt(&["第一批内容".into(), "第二批内容".into()]);
        assert_eq!(prompt, "【第 1 段】\n第一批内容\n【第 2 段】\n第二批内容");
        assert_eq!(OpenAiLlmClient::minutes_prompt(&[]), "");
    }

    /// 回归：超 500 字原文必须被完全覆盖（切成多个窗口），绝不静默丢内容。
    /// 修复前的 `clip_input_window` 只返回首个窗口、丢弃其余——违反"不改意/不丢内容"。
    #[test]
    fn input_windows_cover_full_text_without_loss() {
        // 1200 字无标点：应切成 3 个窗口（500 + 500 + 200），拼接回原文。
        let raw: String = "字".repeat(1200);
        let ws = OpenAiLlmClient::input_windows(&raw);
        assert_eq!(ws.len(), 3, "应切成 3 个窗口");
        assert!(
            ws.iter().all(|w| w.chars().count() <= MAX_INPUT_CHARS),
            "每窗口 ≤500 字"
        );
        assert_eq!(ws.concat(), raw, "拼接后必须与原文逐字一致（不丢内容）");
    }

    /// 回归：带句末标点的长文在标点处切分（句子完整），且仍全覆盖。
    #[test]
    fn input_windows_cut_at_sentence_boundaries_and_cover_all() {
        // 每句 100 字 + 句号，共 6 句 = 600 字 → 切成多窗口，标点边界切断。
        let sentence = format!("{}。", "字".repeat(99));
        let raw = sentence.repeat(6); // 600 字
        let ws = OpenAiLlmClient::input_windows(&raw);
        assert!(ws.len() >= 2, "600 字应切成至少 2 个窗口");
        assert_eq!(ws.concat(), raw, "拼接后必须与原文一致");
        // 每个窗口都应以句末标点或全文结尾
        for w in &ws {
            assert!(
                w.ends_with('。') || w == ws.last().unwrap(),
                "窗口应在句末标点处切断（最后一个窗口可为文尾）"
            );
        }
    }

    /// 短暂文本（≤500 字）原样单窗口。
    #[test]
    fn input_windows_short_text_single_window() {
        let raw = "你好，简短的一句话。";
        let ws = OpenAiLlmClient::input_windows(raw);
        assert_eq!(ws, vec![raw.to_string()]);
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        if haystack.len() < needle.len() {
            return None;
        }
        (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
    }

    /// 回归：带思考标签的模型不能把 `<think>...</think>` 泄露到实时字幕。
    #[test]
    fn cleanup_streaming_hides_reasoning_tags() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut stream, _) = listener.accept().unwrap();

            // 等完整请求体读完再写响应，避免与客户端写入交错导致连接错误。
            let mut request = Vec::new();
            let mut buf = [0_u8; 4096];
            loop {
                let n = stream.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                let Some(header_end) = find_subslice(&request, b"\r\n\r\n") else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]).to_lowercase();
                let content_length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }

            let body = concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: text/event-stream\r\n",
                "Connection: close\r\n\r\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"<think>reasoning</think>\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\" 哈喽，你好。\"}}]}\n\n",
                "data: [DONE]\n\n",
            );
            let _ = stream.write_all(body.as_bytes());
        });

        let config = LlmConfig {
            base_url: format!("http://{addr}"),
            api_key: "test-key".to_string(),
            model: "thinking-model".to_string(),
            persona: None,
        };
        let client = OpenAiLlmClient::new(config);
        let mut partials = Vec::new();
        let cleaned = client
            .cleanup_streaming("哈喽", &mut |partial| partials.push(partial.to_string()))
            .unwrap();

        assert_eq!(cleaned, "哈喽，你好。");
        assert!(
            partials
                .iter()
                .all(|p| !p.contains("think") && !p.contains("reasoning")),
            "所有流式增量都必须隐藏思考内容: {partials:?}"
        );
    }

    /// 回归：返回给前端的模型摘要必须使用 camelCase 字段。
    #[test]
    fn model_summary_serializes_with_camel_case() {
        let value = serde_json::to_value(LlmModelSummary {
            id: "model-a".to_string(),
            owned_by: Some("openai".to_string()),
        })
        .unwrap();
        assert_eq!(value["id"], "model-a");
        assert_eq!(value["ownedBy"], "openai");
    }

    /// 回归：流式未闭合的思考内容必须保持隐藏，且不能闪出半截标签。
    #[test]
    fn strips_partial_reasoning_stream() {
        assert_eq!(strip_reasoning_tags("<think>reasoning"), "");
        assert_eq!(strip_reasoning_tags("<thi"), "");
        assert_eq!(
            strip_reasoning_tags("<think>reasoning</think> 哈喽，你好。"),
            "哈喽，你好。"
        );
    }

    /// 回归：获取模型列表必须请求 `/models` 并携带 Authorization。
    #[test]
    fn lists_models_from_openai_compatible_endpoint() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buf = [0_u8; 4096];
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                return;
            }
            request.extend_from_slice(&buf[..n]);
            let request = String::from_utf8_lossy(&request);
            assert!(
                request.starts_with("GET /models HTTP/1.1"),
                "请求路径: {request}"
            );
            assert!(
                request.contains("Authorization: Bearer test-key"),
                "请求头: {request}"
            );

            let body = r#"{"object":"list","data":[{"id":"model-a","owned_by":"openai"}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        });

        let models = list_llm_models(format!("http://{addr}"), "test-key".to_string()).unwrap();
        assert_eq!(
            models,
            vec![LlmModelSummary {
                id: "model-a".to_string(),
                owned_by: Some("openai".to_string())
            }]
        );
    }

    /// 回归：OpenAI 兼容 `/models` 响应必须解析成可选项。
    #[test]
    fn parses_openai_compatible_model_list() {
        let raw = serde_json::json!({
            "object": "list",
            "data": [
                { "id": "model-a", "object": "model", "owned_by": "openai" },
                { "id": "model-b", "object": "model", "owned_by": "system" }
            ]
        });
        let models = parse_model_list_response(&raw.to_string()).unwrap();
        assert_eq!(
            models,
            vec![
                LlmModelSummary {
                    id: "model-a".to_string(),
                    owned_by: Some("openai".to_string())
                },
                LlmModelSummary {
                    id: "model-b".to_string(),
                    owned_by: Some("system".to_string())
                },
            ]
        );
    }
}
