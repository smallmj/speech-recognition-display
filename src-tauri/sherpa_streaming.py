#!/usr/bin/env python3
"""
sherpa_streaming.py — sherpa-onnx 流式 ASR sidecar（v0.4 管线重构）。

与 Rust (Tauri) 端通过 stdin/stdout 通信：

  Rust → Python (stdin)
    第 1 行    : JSON 配置行  {"type":"config","sample_rate":16000}
    之后       : 二进制 little-endian float32 PCM（单声道，16kHz）
    stdin EOF  : 触发优雅关闭（flush 当前 final 结果后退出）

  Python → Rust (stdout)  每行一条 NDJSON：
    {"type":"started","streaming":true|false,"model":"...","model_kind":"...",
     "sample_rate":16000,"scd_embedding":true|false,"vad":true|false}
    {"type":"partial","text":"..."}   识别中间结果（边说边出，流式模式）
    {"type":"final","text":"...",
     "embedding":[...],                整段有效语音的 speaker embedding（可选）
     "head_embedding":[...],           句首窗口 embedding（可选，SCD 多窗口投票）
     "tail_embedding":[...],           句尾窗口 embedding（可选）
     "speech_duration":1.23,           该段**有效语音**时长（秒，裁静音后）
     "utt_seq":42}                     本段唯一序号（SCD 追溯修正引用）
    {"type":"error","message":"..."}  错误
    {"type":"stopped"}

## v0.4 关键架构变化（对齐 SCD 改善调研报告 P0/P1）

1. **VAD 切片先行**：ASR 前跑专用 VAD（Silero，`silero_vad.onnx`），以 **VAD 段**
   为「转写 + embedding + 气泡」的统一单位。ASR 端点检测不再是句子边界——
   VAD 判定「段结束」（尾静音 ≈0.3s）时立刻定稿并 reset 流，下一位说话人的
   开头不再被并进上一位的 final（修复「第二人开头挂进前一人末尾」）。
   VAD 模型缺失时降级为旧的 ASR 端点定稿（行为与 v0.3 一致）。
2. **句内多 embedding**：每条 final 附带整段 + 句首窗口 + 句尾窗口三个
   embedding（`speech_seconds` 达标时），Rust 端 `Scd::process_utterance_multi`
   据此做 head/tail/whole 投票（治短句指派不稳）与 mixed 检测。
3. **拆句自愈（P1）**：若头/尾窗口 embedding 互不相似（余弦 < 阈值）且段足够
   长，说明该段疑似混入两人 → 用二分搜索取「左右分离度最大」的切点，把音频
   拆成两半，各自重新识别并以两条 final 输出（含各自 embedding）。
4. **多 ASR 模型族**（按模型目录自动探测，可用 `--model-kind` 强制指定）：
   - transducer（zipformer，2023 双语，有 encoder/decoder/joiner）→ 真流式；
   - paraformer（online bilingual zh-en，有 encoder/decoder、无 joiner）→ 真流式；
   - sense-voice（离线，单 model.*.onnx + tokens.txt）→ VAD 段级离线识别，
     高精度模式（use_itn=True，数字/标点归一）。

用法（独立运行，验证 ASR 链路）：
  python3 sherpa_streaming.py \
      --model-dir ./asr-models/<model-dir> \
      --wav ./asr-models/.../test_wavs/0.wav \
      > /tmp/asr.out

SCD speaker embedding 模型（可选）：
  --embedding-model-dir 指向 3d-speaker 等 sherpa-onnx speaker embedding 模型目录
  （目录内含一个 *.onnx）。配置后每个 final 事件携带该段音频的 embedding 向量，
  Rust 端据此做说话人余弦匹配；未配置/加载失败则 final 无 embedding 字段，
  Rust 端 SCD 自动降级为单说话人（不会因缺模型而崩溃）。

VAD 模型（可选，强烈建议）：
  --vad-model 指向 silero_vad.onnx（约 628KB）。提供后启用 VAD 切片。
"""

from __future__ import annotations

import argparse
import errno
import json
import re
import sys
import wave
from pathlib import Path

# 让 `python3 sherpa_streaming.py` 也能用上项目 venv 里的 sherpa_onnx（调试便利）。
_venv_pkgs = Path(__file__).resolve().parent / ".venv" / "lib" / "python3.14" / "site-packages"
if _venv_pkgs.is_dir() and str(_venv_pkgs) not in sys.path:
    sys.path.insert(0, str(_venv_pkgs))
_venv_pkgs311 = Path(__file__).resolve().parent / ".venv" / "lib" / "python3.11" / "site-packages"
if _venv_pkgs311.is_dir() and str(_venv_pkgs311) not in sys.path:
    sys.path.insert(0, str(_venv_pkgs311))

import numpy as np  # noqa: E402
import sherpa_onnx  # noqa: E402

CJK = r"\u4e00-\u9fff\u3400-\u4dbf\uf900-\ufaff"
CJK_PUNCT = "，。！？；：、（）【】《》「」『』"

# ---------------------------------------------------------------------------
# 文本后处理：把 BPE 分词的空格合并成可读文本（中文不空格、英文保空格）
# ---------------------------------------------------------------------------


def normalize_bpe_text(text: str) -> str:
    """把 sherpa-onnx BPE 输出（如 "昨 天 是 Monday"）规范为可读文本。"""
    text = re.sub(r"\s+", " ", text).strip()
    # 中文-中文之间去空格
    text = re.sub(rf"(?<=[{CJK}]) (?=[{CJK}])", "", text)
    # 中文与中文标点之间去空格
    text = re.sub(rf"(?<=[{CJK}]) (?=[{CJK_PUNCT}])", "", text)
    text = re.sub(rf"(?<=[{CJK_PUNCT}]) (?=[{CJK}])", "", text)
    return text


# ---------------------------------------------------------------------------
# 流式识别器（OnlineRecognizer，真流式，边说边出；含 paraformer / ctc 家庭）
# ---------------------------------------------------------------------------


class StreamingRecognizer:
    """基于 sherpa-onnx OnlineRecognizer 的真流式识别器。

    - 每次 feed 后对 ready 的 stream 做 decode，把中间结果作为 partial 输出；
    - v0.4：**句子边界由外部 VAD 驱动**（调用方在 VAD 段结束时调用
      `finalize_segment()` 取当前结果并 reset 新句）；未提供 VAD 时降级为
      端点检测（`enable_endpoint_detection`），`maybe_finalize()` 保留 v0.3 行为。
    """

    _KIND = "transducer"

    def _find(self, prefix: str, suffixes: list[str]) -> Path:
        """按前缀 + 后缀候选在模型目录里找文件（兼容官方 *-epoch-*-avg-* 命名）。"""
        for suffix in suffixes:
            for p in self.model_dir.iterdir():
                name = p.name
                if name.startswith(prefix) and name.endswith(suffix):
                    return p
        return self.model_dir / (prefix + suffixes[0])

    def __init__(
        self,
        model_dir: str,
        num_threads: int = 2,
        sample_rate: int = 16000,
        model_kind: str = "auto",
        enable_endpoint: bool = True,
    ):
        self.model_dir = Path(model_dir)
        self.sample_rate = sample_rate
        self.num_threads = num_threads
        self.enable_endpoint = enable_endpoint
        self.kind = self._detect_kind(model_kind)

        if self.kind == "transducer":
            self._init_transducer()
        elif self.kind == "paraformer":
            self._init_paraformer()
        elif self.kind == "ctc":
            self._init_ctc()
        else:
            raise ValueError(f"未知模型族: {self.kind}（auto/transducer/paraformer/ctc/sense-voice）")

        self.stream = self.recognizer.create_stream()

    # -- 模型族探测 ------------------------------------------------

    def _detect_kind(self, override: str) -> str:
        if override != "auto":
            return override
        has_joiner = any(
            p.name.startswith("joiner") and (p.name.endswith(".onnx") or p.name.endswith(".int8.onnx"))
            for p in self.model_dir.iterdir()
        )
        has_encoder = any(
            p.name.startswith("encoder") and p.name.endswith(".onnx")
            for p in self.model_dir.iterdir()
        )
        has_decoder = any(
            p.name.startswith("decoder") and p.name.endswith(".onnx")
            for p in self.model_dir.iterdir()
        )
        has_single_model = any(
            p.name in ("model.onnx", "model.int8.onnx") for p in self.model_dir.iterdir()
        )
        if has_single_model:
            # 单一 model.*.onnx：SenseVoice 或 Zipformer2-CTC——先按 SenseVoice 探测
            # （本项目只内置 SenseVoice；如需 CTC 用 --model-kind ctc 强制）。
            tokens = self.model_dir / "tokens.txt"
            if tokens.is_file():
                return "sense-voice"
            return "ctc"
        if has_encoder and has_decoder:
            if not has_joiner:
                return "paraformer"  # 无 joiner 的 online paraformer
            return "transducer"
        if has_joiner:
            return "transducer"
        raise FileNotFoundError(
            f"无法识别模型族（目录 {self.model_dir}）：期望 encoder/decoder[/joiner] 或 model.*.onnx + tokens.txt"
        )

    def _require_files(self, files: list[Path]) -> None:
        missing = [str(p) for p in files if not p.is_file()]
        if missing:
            raise FileNotFoundError(f"模型文件缺失: {missing}（目录 {self.model_dir}）")

    def _init_transducer(self):
        encoder = self._find("encoder", [".int8.onnx", ".onnx"])
        decoder = self._find("decoder", [".onnx", ".int8.onnx"])
        joiner = self._find("joiner", [".int8.onnx", ".onnx"])
        tokens = self.model_dir / "tokens.txt"
        self._require_files([encoder, decoder, joiner, tokens])

        bpe = self.model_dir / "bpe.model"
        has_bpe = bpe.is_file()
        kwargs = dict(
            tokens=str(tokens),
            encoder=str(encoder),
            decoder=str(decoder),
            joiner=str(joiner),
            num_threads=self.num_threads,
            sample_rate=int(self.sample_rate),
            feature_dim=80,
            decoding_method="greedy_search",
            enable_endpoint_detection=self.enable_endpoint,
            rule1_min_trailing_silence=1.2,
            rule2_min_trailing_silence=0.6,
            rule3_min_utterance_length=12.0,
        )
        if has_bpe:
            kwargs["modeling_unit"] = "bpe"
            kwargs["bpe_vocab"] = str(bpe)
        else:
            kwargs["modeling_unit"] = "cjkchar"
        self.recognizer = sherpa_onnx.OnlineRecognizer.from_transducer(**kwargs)

    def _init_paraformer(self):
        encoder = self._find("encoder", [".int8.onnx", ".onnx"])
        decoder = self._find("decoder", [".int8.onnx", ".onnx"])
        tokens = self.model_dir / "tokens.txt"
        self._require_files([encoder, decoder, tokens])
        self.recognizer = sherpa_onnx.OnlineRecognizer.from_paraformer(
            tokens=str(tokens),
            encoder=str(encoder),
            decoder=str(decoder),
            num_threads=self.num_threads,
            sample_rate=int(self.sample_rate),
            enable_endpoint_detection=self.enable_endpoint,
            rule1_min_trailing_silence=1.2,
            rule2_min_trailing_silence=0.6,
            rule3_min_utterance_length=12.0,
            decoding_method="greedy_search",
        )

    def _init_ctc(self):
        model = self._find("model", [".int8.onnx", ".onnx"])
        tokens = self.model_dir / "tokens.txt"
        self._require_files([model, tokens])
        self.recognizer = sherpa_onnx.OnlineRecognizer.from_zipformer2_ctc(
            tokens=str(tokens),
            model=str(model),
            num_threads=self.num_threads,
            sample_rate=int(self.sample_rate),
            enable_endpoint_detection=self.enable_endpoint,
            decoding_method="greedy_search",
        )

    # -- feed + decode ----------------------------------------------

    def feed(self, samples: np.ndarray):
        self.stream.accept_waveform(self.sample_rate, samples)
        return self.decode()

    def decode(self) -> str:
        """decode ready 数据，返回当前 partial 文本（未定稿）。"""
        while self.recognizer.is_ready(self.stream):
            self.recognizer.decode_stream(self.stream)
        return normalize_bpe_text(self.recognizer.get_result(self.stream))

    def current_text(self) -> str:
        """取当前流结果（VAD 驱动定稿用，不 reset）。"""
        return normalize_bpe_text(self.recognizer.get_result(self.stream))

    def reset(self):
        """开启新句（VAD 段结束调用：文本边界与 VAD 段边界对齐）。"""
        self.recognizer.reset(self.stream)

    def maybe_finalize(self) -> str | None:
        """（v0.3 降级路径）端点检测触发时返回定稿文本并 reset 新句。"""
        if self.recognizer.is_endpoint(self.stream):
            text = normalize_bpe_text(self.recognizer.get_result(self.stream))
            self.recognizer.reset(self.stream)
            return text
        return None

    def finish(self) -> str | None:
        """输入结束：补尾部静音强制出最终结果。返回定稿文本。"""
        # 补足一个完整解码块（模型 chunk）+ 余量，让尾部词完整出结果
        tail = np.zeros(int(1.5 * self.sample_rate), dtype=np.float32)
        self.stream.accept_waveform(self.sample_rate, tail)
        self.stream.input_finished()
        while self.recognizer.is_ready(self.stream):
            self.recognizer.decode_stream(self.stream)
        text = normalize_bpe_text(self.recognizer.get_result(self.stream))
        return text or None

    def recognize_audio(self, samples: np.ndarray) -> str:
        """对一段独立音频做**离线式**识别（VAD 段定稿 / 拆句自愈的半段用）。

        新建临时流，先喂 0.5s 静音作为**左侧上下文**（流式 zipformer 需要
        left-context 帧才能出字——直接喂从静音后开始的段会得到空结果，实测），
        再按 100ms 块喂入段音频，结束补 1.5s 静音 + `input_finished` 强制出
        结果。不触碰主流的连续状态（主流的 partial 体验不受影响）。
        """
        stream = self.recognizer.create_stream()
        # 左侧上下文：静音垫 + 对齐流式模型 chunk（30ms 帧 × ~16 左帧 ≈ 0.5s）
        stream.accept_waveform(self.sample_rate, np.zeros(int(0.5 * self.sample_rate), dtype=np.float32))
        chunk = 1600  # 100ms @16k
        for i in range(0, len(samples), chunk):
            stream.accept_waveform(self.sample_rate, samples[i : i + chunk])
            while self.recognizer.is_ready(stream):
                self.recognizer.decode_stream(stream)
        # 补尾静音 + input_finished：让尾部音节完整出结果
        tail = np.zeros(int(1.5 * self.sample_rate), dtype=np.float32)
        stream.accept_waveform(self.sample_rate, tail)
        stream.input_finished()
        while self.recognizer.is_ready(stream):
            self.recognizer.decode_stream(stream)
        return normalize_bpe_text(self.recognizer.get_result(stream))


# ---------------------------------------------------------------------------
# 离线段识别器（SenseVoice 高精度模式：VAD 段 → OfflineRecognizer）
# ---------------------------------------------------------------------------


class OfflineSegmentRecognizer:
    """基于 OfflineRecognizer（SenseVoice）的「模拟流式」识别器。

    每个 VAD 段独立识别：段级延迟（≈整句长度，2–4s），换 100% 准确率 +
    标点 + ITN（数字归一）。仅在用户选择「高精度模式」时启用。
    """

    _KIND = "sense-voice"

    def __init__(self, model_dir: str, num_threads: int = 2, sample_rate: int = 16000):
        self.model_dir = Path(model_dir)
        self.sample_rate = sample_rate
        model = self.model_dir / "model.int8.onnx"
        if not model.is_file():
            model = self.model_dir / "model.onnx"
        tokens = self.model_dir / "tokens.txt"
        if not model.is_file() or not tokens.is_file():
            raise FileNotFoundError(
                f"SenseVoice 模型不完整，期望 {self.model_dir} 内含 model.int8.onnx + tokens.txt"
            )
        self.recognizer = sherpa_onnx.OfflineRecognizer.from_sense_voice(
            model=str(model),
            tokens=str(tokens),
            num_threads=num_threads,
            sample_rate=int(sample_rate),
            decoding_method="greedy_search",
            use_itn=True,  # ITN：数字/金额/日期归一（五千八百块 → 5800块）
        )

    def recognize(self, samples: np.ndarray) -> str:
        stream = self.recognizer.create_stream()
        stream.accept_waveform(self.sample_rate, samples)
        self.recognizer.decode_stream(stream)
        text = self.recognizer.get_result(stream).strip()
        return re.sub(r"\s+", " ", text)


# ---------------------------------------------------------------------------
# 说话人 speaker embedding 提取（T5 SCD + v0.4 多窗口）：可选，缺模型优雅降级
# ---------------------------------------------------------------------------


def trim_trailing_silence(samples: np.ndarray, threshold: float = 1e-4) -> np.ndarray:
    """去掉句尾静音。

    端点检测/VAD 的段会包含触发判定所需的尾部静音,直接提 embedding 会掺入
    无用静音帧，这里按幅值阈值裁掉尾部近零采样，只保留有效语音段。
    """
    if samples.size == 0:
        return samples
    nz = np.flatnonzero(np.abs(samples) > threshold)
    if nz.size == 0:
        return samples
    return samples[: int(nz[-1]) + 1]


class SpeakerEmbedder:
    """sherpa-onnx 说话人 speaker embedding 提取器（3d-speaker 等）。

    v0.4 增加多窗口计算：`compute_segment_embeddings(samples, window_seconds)`
    返回 (whole, head, tail) 三个向量，供 Rust `Scd::process_utterance_multi`
    做 head/tail/whole 投票与 mixed 检测。
    """

    def __init__(self, model_dir: str, num_threads: int = 2, sample_rate: int = 16000):
        self.extractor = None
        self.sample_rate = sample_rate
        if not model_dir:
            return
        p = Path(model_dir)
        if not p.is_dir():
            print(f"[sherpa_streaming] speaker embedding 模型目录不存在，SCD 降级: {p}", file=sys.stderr)
            return
        onnx = next((f for f in p.iterdir() if f.suffix == ".onnx"), None)
        if onnx is None:
            print(f"[sherpa_streaming] speaker embedding 模型目录内无 .onnx，SCD 降级: {p}", file=sys.stderr)
            return
        try:
            if hasattr(sherpa_onnx, "SpeakerEmbeddingExtractorConfig"):
                config = sherpa_onnx.SpeakerEmbeddingExtractorConfig(
                    model=str(onnx),
                    num_threads=num_threads,
                    debug=False,
                )
                self.extractor = sherpa_onnx.SpeakerEmbeddingExtractor(config)
            else:
                self.extractor = sherpa_onnx.SpeakerEmbeddingExtractor(
                    model=str(onnx),
                    num_threads=num_threads,
                    debug=False,
                )
        except Exception as e:  # noqa: BLE001
            print(f"[sherpa_streaming] 加载 speaker embedding 模型失败，SCD 降级: {e}", file=sys.stderr)
            self.extractor = None

    @property
    def available(self) -> bool:
        return self.extractor is not None

    def compute(self, samples: np.ndarray) -> list[float] | None:
        """对一段音频提取说话人 speaker embedding；失败返回 None（调用方降级）。"""
        if self.extractor is None:
            return None
        if samples.size == 0:
            return None
        try:
            stream = self.extractor.create_stream()
            stream.accept_waveform(self.sample_rate, samples)
            if hasattr(stream, "input_finished"):
                stream.input_finished()
            emb = None
            while self.extractor.is_ready(stream):
                emb = self.extractor.compute(stream)
            if emb is None and hasattr(self.extractor, "get_result"):
                emb = self.extractor.get_result(stream)
            if not emb:
                return None
            return [float(x) for x in emb]
        except Exception as e:  # noqa: BLE001
            print(f"[sherpa_streaming] 提取 embedding 失败，该段降级: {e}", file=sys.stderr)
            return None

    def compute_segment_embeddings(
        self, samples: np.ndarray, window_seconds: float = 1.0
    ) -> tuple[list[float] | None, list[float] | None, list[float] | None]:
        """(whole, head, tail) 三窗口 embedding。

        - whole：整段有效语音（裁尾静音后）；
        - head：句首 window_seconds 秒；
        - tail：句尾 window_seconds 秒（裁尾静音后取末尾窗口）。
        任一窗口过短（< 0.25s）返回 None（不做无意义的小窗 embedding）。
        """
        if samples.size == 0:
            return (None, None, None)
        trimmed = trim_trailing_silence(samples)
        whole = self.compute(trimmed)

        win = int(window_seconds * self.sample_rate)
        head_emb, tail_emb = None, None
        if trimmed.size >= win * 2:  # 段足够长才值得分窗口
            head_samples = trimmed[:win]
            if head_samples.size >= int(0.25 * self.sample_rate):
                head_emb = self.compute(head_samples)
            tail_samples = trimmed[-win:]
            if tail_samples.size >= int(0.25 * self.sample_rate):
                tail_emb = self.compute(tail_samples)
        return (whole, head_emb, tail_emb)


# ---------------------------------------------------------------------------
# VAD（v0.4 P0）：Silero VAD 段落检测 —— 句子边界统一由 VAD 提供
# ---------------------------------------------------------------------------


class VadSegmenter:
    """silero VAD 包装：把音频流切成「语音段」（转写/embedding/气泡统一单位）。

    参数对齐 SCD 改善调研报告 §4.2：
    - `min_silence_duration` ≈ 0.3s（对话交接敏感的段尾判定）；
    - `threshold` 0.5、`min_speech_duration` 0.25s、`max_speech_duration` 15s
      （超长强制切，兜底极长句）。
    """

    def __init__(self, model_path: str, sample_rate: int = 16000, num_threads: int = 2):
        self.sample_rate = sample_rate
        self.available = False
        if not model_path:
            return
        if not Path(model_path).is_file():
            print(f"[sherpa_streaming] VAD 模型不存在，SCD 边界降级为 ASR 端点: {model_path}", file=sys.stderr)
            return
        try:
            config = sherpa_onnx.VadModelConfig()
            config.sample_rate = sample_rate
            config.num_threads = num_threads
            config.silero_vad = sherpa_onnx.SileroVadModelConfig(
                model=model_path,
                threshold=0.5,
                min_silence_duration=0.3,
                min_speech_duration=0.25,
                max_speech_duration=15.0,
                window_size=512,
            )
            self.vad = sherpa_onnx.VoiceActivityDetector(config, buffer_size_in_seconds=30)
            self.available = True
        except Exception as e:  # noqa: BLE001
            print(f"[sherpa_streaming] 加载 VAD 模型失败，降级为 ASR 端点: {e}", file=sys.stderr)
            self.vad = None
            self.available = False

    def accept(self, samples: np.ndarray):
        if self.available:
            try:
                # 注意：VoiceActivityDetector.accept_waveform 的绑定只接收 samples
                # （采样率在 VadModelConfig.sample_rate 里配置，不再传参）。
                self.vad.accept_waveform(samples)
            except Exception as e:  # noqa: BLE001
                print(f"[sherpa_streaming] VAD accept 失败，该块跳过: {e}", file=sys.stderr)

    def pop_segments(self) -> list[np.ndarray]:
        """取走所有已结束的 VAD 段（每段为 float32 音频）。

        注意绑定差异：`front` 是属性（不是方法，`vad.front` 返回
        `SpeechSegment`，有 `samples`/`start`）；`pop()` 是方法。
        """
        out = []
        if not self.available:
            return out
        try:
            while not self.vad.empty():
                seg = self.vad.front  # 属性：SpeechSegment
                samples = np.asarray(seg.samples, dtype=np.float32).copy()
                self.vad.pop()
                out.append(samples)
        except Exception as e:  # noqa: BLE001
            print(f"[sherpa_streaming] 取 VAD 段失败: {e}", file=sys.stderr)
        return out


# ---------------------------------------------------------------------------
# 拆句自愈（P1）：head/tail 互不相似 → 二分切点 → 两条 final
# ---------------------------------------------------------------------------


def cosine(a: np.ndarray, b: np.ndarray) -> float:
    na = float(np.linalg.norm(a)) if a.size else 0.0
    nb = float(np.linalg.norm(b)) if b.size else 0.0
    if na == 0.0 or nb == 0.0:
        return 0.0
    return float(np.dot(a, b) / (na * nb))


# ---------------------------------------------------------------------------
# 后台回补订正（P1）：对近端 VAD 段的 embedding 做 FastClustering，
# 用「聚类赢家」与当前归属不一致的段下发 backfill 修正（不依赖新模型）。
# ---------------------------------------------------------------------------


class BackfillClusterer:
    """滚动窗口 embedding 聚类回补。

    - 维护最近的 `window_size` 条 final（utt_seq → 整段 embedding）；
    - 新 final 入桶后，若自上次回补以来累积了 ≥ `min_batch` 条，跑一次
      FastClustering（`num_clusters=-1, threshold≈0.5` 自动定簇）；
    - 输出每个段的簇标签，Rust 端收到 `backfill` 事件后，把「簇赢家说话人」
      与当前归属比对，不一致的段发 `SpeakerCorrected` 订正（只改标签，不改原文）。
    - embedding 不可用（SCD 降级单说话人）时静默禁用——与主 SCD 路径同款降级。
    """

    def __init__(
        self,
        sample_rate: int = 16000,
        window_size: int = 40,
        min_batch: int = 12,
        threshold: float = 0.5,
    ):
        self.sample_rate = sample_rate
        self.window_size = window_size
        self.min_batch = min_batch
        self.threshold = threshold
        self.available = True
        self._seqs: list[int] = []
        self._embs: list[np.ndarray] = []
        self._since_last_check = 0

    def reset(self):
        self._seqs = []
        self._embs = []
        self._since_last_check = 0

    def push(self, utt_seq: int, whole_emb: list[float] | None) -> list[dict] | None:
        """入桶一条 final；触发回补时返回 `[{seq, cluster}]`，否则 None。"""
        if not self.available or whole_emb is None:
            return None
        self._seqs.append(utt_seq)
        self._embs.append(np.asarray(whole_emb, dtype=np.float32))
        if len(self._embs) > self.window_size:
            del self._seqs[: len(self._embs) - self.window_size]
            del self._embs[: len(self._embs) - self.window_size]
        self._since_last_check += 1
        if self._since_last_check < self.min_batch or len(self._embs) < self.min_batch:
            return None
        self._since_last_check = 0
        try:
            mat = np.stack(self._embs)
            cfg = sherpa_onnx.FastClusteringConfig(
                num_clusters=-1, threshold=self.threshold
            )
            clustering = sherpa_onnx.FastClustering(cfg)
            labels = clustering(mat)
        except Exception as e:  # noqa: BLE001
            print(f"[sherpa_streaming] 回补聚类失败，跳过本轮: {e}", file=sys.stderr)
            return None
        return [
            {"seq": seq, "cluster": int(label)}
            for seq, label in zip(self._seqs, labels)
        ]


def find_best_split(
    samples: np.ndarray,
    embedder: SpeakerEmbedder,
    sample_rate: int,
    min_split_seconds: float = 0.6,
) -> int | None:
    """在段内二分搜索「左右分离度最大」的切点（采样数）。

    只对 head/tail 已判定为「疑似两人」的段调用（见主循环）；候选切点
    0.35/0.45/0.55/0.65 比例处，取 1-cos(left,right) 最大者。返回切点采样
    下标（保证左右各 ≥ min_split_seconds），找不到合格切点返回 None。
    """
    total = samples.size
    min_len = int(min_split_seconds * sample_rate)
    if total < min_len * 2:
        return None
    best_cut, best_score = None, 0.0
    for frac in (0.35, 0.45, 0.55, 0.65):
        cut = int(total * frac)
        if cut < min_len or (total - cut) < min_len:
            continue
        left = embedder.compute(samples[:cut]) if samples[:cut].size else None
        right = embedder.compute(samples[cut:]) if samples[cut:].size else None
        if left is None or right is None:
            continue
        score = 1.0 - cosine(np.asarray(left, dtype=np.float32), np.asarray(right, dtype=np.float32))
        if score > best_score:
            best_score, best_cut = score, cut
    if best_cut is not None and best_score > 0.55:  # 分离度充分大才拆（保守）
        return best_cut
    return None


# ---------------------------------------------------------------------------
# 主循环：stdin 协议
# ---------------------------------------------------------------------------


def emit(obj: dict):
    # 强制 UTF-8 输出到 stdout.buffer：Windows 的 sys.stdout 默认编码是 GBK/cp1252，
    # 用 write() 会让中文按本地编码输出（Rust 端按 UTF-8 解码失败 → 丢弃整行）。
    sys.stdout.buffer.write(json.dumps(obj, ensure_ascii=False).encode("utf-8") + b"\n")
    try:
        sys.stdout.buffer.flush()
    except OSError as e:
        # Windows 管道模式下 flush 可能报 "Invalid argument"(EINVAL)：数据已写入
        # 内核缓冲区，Tauri 端 read 时仍能收到，忽略该特定错误即可；其余 OSError
        # （如管道已断 EPIPE）照常抛出，避免掩盖真实的 I/O 交付失败。
        if e.errno != errno.EINVAL:
            raise


def final_event(
    text: str,
    seg_samples: np.ndarray | None,
    embedder: SpeakerEmbedder | None,
    sample_rate: int = 16000,
    window_seconds: float = 1.0,
    utt_seq: int = 0,
) -> dict:
    """构造一条 final 事件（v0.4：多窗口 embedding + 有效语音时长 + 序号）。"""
    obj = {"type": "final", "text": text, "utt_seq": utt_seq}
    if seg_samples is not None and seg_samples.size:
        samples = trim_trailing_silence(seg_samples)
        obj["speech_duration"] = round(len(samples) / float(sample_rate), 3)
        if embedder is not None and embedder.available:
            whole, head, tail = embedder.compute_segment_embeddings(samples, window_seconds)
            if whole:
                obj["embedding"] = whole
            if head:
                obj["head_embedding"] = head
            if tail:
                obj["tail_embedding"] = tail
    return obj


def run_streaming(
    stdin,
    model_dir: str,
    sample_rate: int,
    embedding_model_dir: str = "",
    vad_model: str = "",
    model_kind: str = "auto",
    split_threshold: float = 0.30,
    window_seconds: float = 1.0,
):
    # -- 读配置行：逐字节直到换行，避免把后续二进制音频吞进缓冲
    line = b""
    while True:
        b = stdin.read(1)
        if not b or b == b"\n":
            break
        line += b
    if line:
        try:
            cfg = json.loads(line.decode("utf-8"))
            sample_rate = int(cfg.get("sample_rate", sample_rate))
            if "model_kind" in cfg:
                model_kind = cfg.get("model_kind", model_kind)
        except (ValueError, json.JSONDecodeError):
            emit({"type": "error", "message": f"无法解析配置行: {line!r}"})
            sys.exit(1)

    # 模型族探测：SenseVoice（离线）与流式家庭分开构建
    kind = model_kind
    if kind == "auto":
        # 先探测：单 model.*.onnx + tokens.txt → sense-voice；否则交给
        # StreamingRecognizer 的 _detect_kind（transducer/paraformer/ctc 依
        # encoder/decoder/joiner 结构区分）。这里不能直接定 "transducer"——
        # paraformer 目录有 encoder+decoder 无 joiner，会被误判并报缺 joiner。
        md = Path(model_dir)
        single_model = any(p.name in ("model.onnx", "model.int8.onnx") for p in md.iterdir())
        kind = "sense-voice" if single_model else "auto"

    if kind == "sense-voice":
        rec = OfflineSegmentRecognizer(model_dir=model_dir, sample_rate=sample_rate)
        streaming = False
    else:
        rec = StreamingRecognizer(
            model_dir=model_dir,
            sample_rate=sample_rate,
            model_kind=kind,  # 传 "auto" 时由 _detect_kind 按文件结构准确探测
            enable_endpoint=(vad_model == ""),  # 无 VAD 时启用端点兜底
        )
        kind = rec.kind
        streaming = True

    embedder = SpeakerEmbedder(model_dir=embedding_model_dir, sample_rate=sample_rate)
    vad = VadSegmenter(vad_model, sample_rate=sample_rate)
    # v0.4 P1：后台回补聚类（embedding 可用时自动启用；降级单说话人时静默禁用）。
    backfill = BackfillClusterer(sample_rate=sample_rate)
    emit(
        {
            "type": "started",
            "streaming": streaming,
            "model": rec.model_dir.name,
            "model_kind": kind,
            "sample_rate": rec.sample_rate,
            "scd_embedding": embedder.available,
            "vad": vad.available,
            "backfill": embedder.available,
        }
    )

    bytes_per_sample = 4  # float32
    buf = b""
    last_partial = ""
    # final 序号（SCD 追溯修正引用；Rust 端 utt_seq → segment_id 映射）。
    utt_seq = 0
    # 无 VAD 降级路径：累积「自上次端点 final 以来」的音频（供 embedding 提取）。
    streaming_seg_buf: list[np.ndarray] = []
    # VAD 段定稿队列：VAD 说段结束 → 入队 (剩余块数, 段音频)；等解码滞后消化
    # （settle 窗口 ≈ 400ms）后取主流文本并 reset，保证「文本落点 ≈ VAD 边界」。
    # 方案对齐调研 §4.3 方案 1：保留流式 ASR（转写质量最优），VAD 只当气泡边界。
    from collections import deque  # noqa: PLC0415
    pending: deque = deque()

    def emit_final_for_segment(seg_audio):
        """处理一条已定稿的 VAD 段：取文本 → 拆句自愈 → 发 final（含多窗口 embedding）。"""
        nonlocal utt_seq, last_partial
        if seg_audio.size < int(0.25 * sample_rate):
            return  # 过短段（半个气音）：不产出气泡，避免碎片（调研 P0 护栏）
        text = normalize_bpe_text(rec.current_text())
        last_partial = ""
        if not text.strip():
            return
        # 整段 embedding（拆句自愈 + 后台回补共用；embedding 不可用时为 None）
        whole = head = tail = None
        if embedder.available:
            whole, head, tail = embedder.compute_segment_embeddings(seg_audio, window_seconds)
        # 拆句自愈：head/tail 互不相似（疑似两说话人）且段足够长
        if (
            whole
            and head
            and tail
            and np.linalg.norm(np.asarray(head, dtype=np.float32)) > 0
            and np.linalg.norm(np.asarray(tail, dtype=np.float32)) > 0
            and cosine(
                np.asarray(head, dtype=np.float32),
                np.asarray(tail, dtype=np.float32),
            )
            < split_threshold
            and len(seg_audio) >= int(2.2 * sample_rate)
        ):
            cut = find_best_split(seg_audio, embedder, sample_rate)
            if cut is not None and cut > 0 and cut < len(seg_audio):
                left, right = seg_audio[:cut], seg_audio[cut:]
                left_text = rec.recognize_audio(left) if streaming else rec.recognize(left)
                right_text = rec.recognize_audio(right) if streaming else rec.recognize(right)
                utt_seq += 1
                emit(
                    final_event(
                        left_text, left, embedder, sample_rate, window_seconds, utt_seq
                    )
                )
                backfill.push(utt_seq, whole)
                utt_seq += 1
                emit(
                    final_event(
                        right_text, right, embedder, sample_rate, window_seconds, utt_seq
                    )
                )
                backfill.push(utt_seq, tail)
                return
        utt_seq += 1
        emit(
            final_event(
                text, seg_audio, embedder, sample_rate, window_seconds, utt_seq
            )
        )
        # 后台回补：入桶该段 embedding，触发时下发 backfill 事件
        updates = backfill.push(utt_seq, whole)
        if updates:
            emit({"type": "backfill", "update": updates})

    SETTLE_CHUNKS = 4  # 400ms：覆盖流式解码的尾部滞后，又尽量短以免吞进下一人开头

    while True:
        chunk = stdin.read(1600 * bytes_per_sample)  # 100ms @16k
        if not chunk:
            break
        buf += chunk
        if len(buf) >= 1600 * bytes_per_sample:
            n = (len(buf) // (1600 * bytes_per_sample)) * (1600 * bytes_per_sample)
            samples = np.frombuffer(buf[:n], dtype=np.float32).copy()
            buf = buf[n:]

            # VAD 先行：段事件是句子边界；流式 ASR 持续消费（partial 边说边出）。
            vad.accept(samples)
            if streaming:
                partial = rec.feed(samples)
                if partial and partial != last_partial:
                    emit({"type": "partial", "text": partial})
                    last_partial = partial

            if vad.available:
                # 新结束的 VAD 段 → 入队（settle 窗口后定稿）
                for seg_audio in vad.pop_segments():
                    if seg_audio.size >= int(0.25 * sample_rate):
                        pending.append([SETTLE_CHUNKS, seg_audio])
                # FIFO settle：队头到期才定稿（保持 VAD 边界顺序）
                if pending and pending[0][0] <= 0:
                    rec.decode()
                    _, seg_audio = pending.popleft()
                    emit_final_for_segment(seg_audio)
                    if streaming and hasattr(rec, "reset"):
                        rec.reset()
                for item in pending:
                    item[0] -= 1

            # 无 VAD（降级）→ 走 ASR 端点定稿（v0.3 行为：该句音频一并提取 embedding）。
            if not vad.available and streaming:
                # 累积「自上次 final 以来」的音频，给端点定稿段提 embedding（保持
                # SCD 在降级路径仍可用；v0.3 原行为）。注意必须 append「数组」，
                # 不能 list(samples)（会变成标量列表，concat 报 zero-dimensional）。
                streaming_seg_buf.append(samples)
                final = rec.maybe_finalize()
                if final:
                    seg_audio = (
                        np.concatenate(streaming_seg_buf) if streaming_seg_buf else None
                    )
                    streaming_seg_buf = []
                    emit(
                        final_event(
                            final,
                            seg_audio,
                            embedder,
                            sample_rate,
                            window_seconds,
                            utt_seq,
                        )
                    )
                    utt_seq += 1
                    last_partial = ""

    # stdin EOF → 优雅关闭：把剩余 pending 段全部定稿
    if vad.available:
        rec.decode()
        while pending:
            _, seg_audio = pending.popleft()
            emit_final_for_segment(seg_audio)
            if streaming and hasattr(rec, "reset"):
                rec.reset()
                rec.decode()
        # VAD flush 兜底（最后一段可能还没 pop 完整）
        for seg_audio in vad.pop_segments():
            if seg_audio.size < int(0.25 * sample_rate):
                continue
            emit_final_for_segment(seg_audio)
            if streaming and hasattr(rec, "reset"):
                rec.reset()
                rec.decode()
    else:
        final = rec.finish() if streaming else None
        if final:
            seg_audio = (
                np.concatenate(streaming_seg_buf) if streaming_seg_buf else None
            )
            utt_seq += 1
            emit(
                final_event(
                    final, seg_audio, embedder, sample_rate, window_seconds, utt_seq
                )
            )
    emit({"type": "stopped"})


# ---------------------------------------------------------------------------
# 独立运行（--wav）：不依赖 Rust，直接把识别结果打印到 stdout，验证 ASR 链路
# ---------------------------------------------------------------------------


def run_wav(wav_path: str, model_dir: str, sample_rate: int, model_kind: str = "auto"):
    md = Path(model_dir)
    kind = model_kind
    if kind == "auto":
        # auto 探测交给 StreamingRecognizer/OfflineSegmentRecognizer 各自完成；
        # 这里仅先判断是否 SenseVoice（单 model.*.onnx），其余交给前者探测
        # （transducer / paraformer / ctc 由 encoder/decoder/joiner 结构区分）。
        single_model = any(p.name in ("model.onnx", "model.int8.onnx") for p in md.iterdir())
        kind = "sense-voice" if single_model else "auto"
    if kind == "sense-voice":
        rec = OfflineSegmentRecognizer(model_dir=model_dir, sample_rate=sample_rate)
    else:
        rec = StreamingRecognizer(model_dir=model_dir, sample_rate=sample_rate, model_kind=kind)
    kind = rec.kind if hasattr(rec, "kind") else kind

    with wave.open(wav_path, "rb") as w:
        sr = w.getframerate()
        data = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16).astype(
            np.float32
        ) / 32768.0

    print(f"[sherpa_streaming] wav={wav_path} sr={sr} dur={len(data)/sr:.2f}s kind={kind}", file=sys.stderr)
    chunk = int(0.1 * sr)
    last = ""
    for i in range(0, len(data), chunk):
        if streaming_mode(kind):
            partial = rec.feed(data[i : i + chunk])
            if partial and partial != last:
                print(f"PARTIAL: {partial}", flush=True)
                last = partial
            final = rec.maybe_finalize()
            if final:
                print(f"FINAL: {final}", flush=True)
                last = ""
    if streaming_mode(kind):
        final = rec.finish()
        if final:
            print(f"FINAL: {final}", flush=True)
    else:
        print(f"FINAL: {rec.recognize(data)}", flush=True)


def streaming_mode(kind: str) -> bool:
    return kind != "sense-voice"


def main():
    parser = argparse.ArgumentParser(description="sherpa-onnx 流式 ASR sidecar")
    parser.add_argument(
        "--model-dir",
        required=True,
        help="流式模型目录（transducer: encoder/decoder/joiner + tokens；"
        "paraformer: encoder/decoder + tokens；sense-voice: model.int8.onnx + tokens）",
    )
    parser.add_argument("--sample-rate", type=int, default=16000)
    parser.add_argument(
        "--embedding-model-dir",
        default="",
        help="（可选，T5 SCD）说话人 speaker embedding 模型目录（3d-speaker 等，内含 *.onnx）。"
        "提供后每条 final 附带 embedding/head_embedding/tail_embedding 供 Rust 端说话人匹配；"
        "缺失/加载失败则降级为单说话人",
    )
    parser.add_argument(
        "--vad-model",
        default="",
        help="（可选，v0.4 P0）silero_vad.onnx 路径。提供后启用 VAD 切片（句子边界由 VAD "
        "提供，修复交接处下一人开头并进上一位 final）；缺失则降级为 ASR 端点定稿",
    )
    parser.add_argument(
        "--model-kind",
        default="auto",
        help="模型族强制指定：auto / transducer / paraformer / ctc / sense-voice（默认 auto 探测）",
    )
    parser.add_argument(
        "--split-threshold",
        type=float,
        default=0.30,
        help="（可选，P1 拆句）head/tail embedding 余弦低于此值视为疑似两说话人，尝试拆句",
    )
    parser.add_argument(
        "--window-seconds",
        type=float,
        default=1.0,
        help="（可选）多窗口 embedding 的头/尾窗口长度（秒）",
    )
    parser.add_argument(
        "--wav",
        default="",
        help="独立验证模式：识别该 wav 并打印结果（不需要 Rust stdin）",
    )
    args = parser.parse_args()

    if args.wav:
        run_wav(args.wav, args.model_dir, args.sample_rate, args.model_kind)
    else:
        try:
            run_streaming(
                sys.stdin.buffer,
                args.model_dir,
                args.sample_rate,
                args.embedding_model_dir,
                args.vad_model,
                args.model_kind,
                args.split_threshold,
                args.window_seconds,
            )
        except Exception as e:  # noqa: BLE001
            emit({"type": "error", "message": f"{type(e).__name__}: {e}"})
            sys.exit(1)


if __name__ == "__main__":
    main()