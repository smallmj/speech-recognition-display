#!/usr/bin/env python3
"""
sherpa_streaming.py — sherpa-onnx 流式 ASR sidecar。

与 Rust (Tauri) 端通过 stdin/stdout 通信：

  Rust → Python (stdin)
    第 1 行    : JSON 配置行  {"type":"config","sample_rate":16000}
    之后       : 二进制 little-endian float32 PCM（单声道，16kHz）
    stdin EOF  : 触发优雅关闭（flush 当前 final 结果后退出）

  Python → Rust (stdout)  每行一条 NDJSON：
    {"type":"started","streaming":true,"model":"...","sample_rate":16000,
     "scd_embedding":true|false}                 是否加载了 speaker embedding 模型
    {"type":"partial","text":"..."}   识别中间结果（边说边出）
    {"type":"final","text":"...",
     "embedding":[...]}               一句话定稿（端点/静音触发）；
                                     embedding 字段仅当配置了 speaker embedding
                                     模型时出现（T5 SCD：该段音频的 speaker
                                     embedding，Rust 端余弦匹配）
    {"type":"error","message":"..."}  错误
    {"type":"stopped"}

用法（独立运行，验证 ASR 链路）：
  python3 sherpa_streaming.py \
      --model-dir ./asr-models/sherpa-onnx-x-asr-960ms-streaming-zipformer-transducer-zh-en-punct-int8-2026-06-05 \
      --wav ./asr-models/.../test_wavs/0.wav \
      > /tmp/asr.out

T5 SCD speaker embedding 模型（可选）：
  --embedding-model-dir 指向 3d-speaker 等 sherpa-onnx speaker embedding 模型目录
  （目录内含一个 *.onnx）。配置后每个 final 事件携带该段音频的 embedding 向量，
  Rust 端据此做说话人余弦匹配；未配置/加载失败则 final 无 embedding 字段，
  Rust 端 SCD 自动降级为单说话人（不会因缺模型而崩溃）。
"""

from __future__ import annotations

import argparse
import json
import re
import struct
import sys
import wave
from pathlib import Path

# 让 `python3 sherpa_streaming.py` 也能用上项目 venv 里的 sherpa_onnx（调试便利）。
_venv_pkgs = Path(__file__).resolve().parent / ".venv" / "lib" / "python3.11" / "site-packages"
if _venv_pkgs.is_dir() and str(_venv_pkgs) not in sys.path:
    sys.path.insert(0, str(_venv_pkgs))

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
# 流式识别器（OnlineRecognizer，真流式，边说边出）
# ---------------------------------------------------------------------------


class StreamingRecognizer:
    """基于 sherpa-onnx OnlineRecognizer 的真流式识别器。

    - 每次 feed 后对 ready 的 stream 做 decode，把中间结果作为 partial 输出；
    - 端点检测（enable_endpoint_detection）触发时输出 final，并 reset 开启新句。
    """

    def _find(self, prefix: str, suffixes: list[str]) -> Path:
        """按前缀 + 后缀候选在模型目录里找文件（兼容官方 *-epoch-*-avg-* 命名）。"""
        for suffix in suffixes:
            for p in self.model_dir.iterdir():
                name = p.name
                if name.startswith(prefix) and name.endswith(suffix):
                    return p
        return self.model_dir / (prefix + suffixes[0])

    def __init__(self, model_dir: str, num_threads: int = 2, sample_rate: int = 16000):
        self.model_dir = Path(model_dir)
        self.sample_rate = sample_rate

        # x-asr streaming zipformer transducer（BPE）文件布局
        # 兼容两种命名：官方模型原始名（*-epoch-*-avg-*）或简化名
        encoder = self._find("encoder", [".int8.onnx", ".onnx"])
        decoder = self._find("decoder", [".onnx", ".int8.onnx"])
        joiner = self._find("joiner", [".int8.onnx", ".onnx"])
        tokens = self.model_dir / "tokens.txt"
        bpe = self.model_dir / "bpe.model"

        if not all(p.is_file() for p in (encoder, decoder, joiner, tokens)):
            raise FileNotFoundError(
                f"流式模型文件不完整，期望 {self.model_dir} 内含 "
                "encoder/decoder/joiner 的 .onnx（可带 int8/epoch 后缀）+ tokens.txt"
            )

        # BPE 模型需要 bpe.model；CJK 字符模型（无 bpe.model）用 modeling_unit="cjk"
        bpe = self.model_dir / "bpe.model"
        has_bpe = bpe.is_file()

        recognizer_kwargs = dict(
            tokens=str(tokens),
            encoder=str(encoder),
            decoder=str(decoder),
            joiner=str(joiner),
            num_threads=num_threads,
            sample_rate=int(sample_rate),
            feature_dim=80,
            decoding_method="greedy_search",
            enable_endpoint_detection=True,
            rule1_min_trailing_silence=1.2,
            rule2_min_trailing_silence=0.6,
            rule3_min_utterance_length=12.0,
        )
        if has_bpe:
            recognizer_kwargs["modeling_unit"] = "bpe"
            recognizer_kwargs["bpe_vocab"] = str(bpe)
        else:
            recognizer_kwargs["modeling_unit"] = "cjk"

        self.recognizer = sherpa_onnx.OnlineRecognizer.from_transducer(**recognizer_kwargs)
        self.stream = self.recognizer.create_stream()

    # -- feed + decode ------------------------------------------------------

    def feed(self, samples: np.ndarray):
        self.stream.accept_waveform(self.sample_rate, samples)
        return self.decode()

    def decode(self) -> str:
        """decode ready 数据，返回当前 partial 文本（未定稿）。"""
        while self.recognizer.is_ready(self.stream):
            self.recognizer.decode_stream(self.stream)
        return normalize_bpe_text(self.recognizer.get_result(self.stream))

    def maybe_finalize(self) -> str | None:
        """若检测到端点（一句话说完），返回定稿文本并 reset 新句；否则 None。"""
        if self.recognizer.is_endpoint(self.stream):
            text = normalize_bpe_text(self.recognizer.get_result(self.stream))
            self.recognizer.reset(self.stream)
            return text
        return None

    def finish(self) -> str | None:
        """输入结束：补尾部静音强制出最终结果。返回定稿文本。"""
        # 补足一个完整解码块（模型 chunk=960ms）+ 余量，让尾部词完整出结果
        tail = np.zeros(int(1.5 * self.sample_rate), dtype=np.float32)
        self.stream.accept_waveform(self.sample_rate, tail)
        self.stream.input_finished()
        while self.recognizer.is_ready(self.stream):
            self.recognizer.decode_stream(self.stream)
        text = normalize_bpe_text(self.recognizer.get_result(self.stream))
        return text or None


# ---------------------------------------------------------------------------
# 说话人 speaker embedding 提取（T5 SCD）：可选，模型缺失时优雅降级
# ---------------------------------------------------------------------------


def trim_trailing_silence(samples: np.ndarray, threshold: float = 1e-4) -> np.ndarray:
    """去掉句尾静音。

    端点检测（enable_endpoint_detection）的 final 会包含触发判定所需的尾部
    静音（rule1/rule2 的 trailing silence），直接提 embedding 会掺入无用静音帧，
    这里按幅值阈值裁掉尾部近零采样，只保留有效语音段。
    """
    if samples.size == 0:
        return samples
    nz = np.flatnonzero(np.abs(samples) > threshold)
    if nz.size == 0:
        return samples
    return samples[: int(nz[-1]) + 1]


class SpeakerEmbedder:
    """sherpa-onnx 说话人 speaker embedding 提取器（3d-speaker / wav2vec2 speaker 系列）。

    - 构造时加载模型目录内第一个 `*.onnx`；模型缺失/加载失败 → `available=False`，
      调用方不输出 embedding，Rust 端 SCD 相应降级为单说话人。
    - `compute(samples)` 对一段 16kHz 音频返回 speaker embedding（list[float]）。

    注：sherpa-onnx Python 绑定 API（SpeakerEmbeddingExtractor / create_stream /
    is_ready / compute）以本机安装的 sherpa-onnx 版本为准：1.13.x 的构造器接收
    `SpeakerEmbeddingExtractorConfig`，`compute(stream)` 直接返回 embedding 向量
    （旧版用 `get_result`）；此处做版本兼容，全部调用包在 try/except 内，任何
    API 差异都只会导致该段降级（不输出 embedding），不会拖垮识别主流程。
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
            # 1.13+ 需要显式构造 config 对象；旧版直接传关键字。优先新 API。
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
        try:
            stream = self.extractor.create_stream()
            stream.accept_waveform(self.sample_rate, samples)
            # 信号输入结束；对 is_ready 触发非必需，但语义正确且无害。
            if hasattr(stream, "input_finished"):
                stream.input_finished()
            emb = None
            # 1.13+：compute 每次返回 embedding 向量，取最后一次（完整段）。
            while self.extractor.is_ready(stream):
                emb = self.extractor.compute(stream)
            # 旧版兼容：compute 后从 get_result 取向量。
            if emb is None and hasattr(self.extractor, "get_result"):
                emb = self.extractor.get_result(stream)
            if not emb:
                return None
            return [float(x) for x in emb]
        except Exception as e:  # noqa: BLE001
            print(f"[sherpa_streaming] 提取 embedding 失败，该段降级: {e}", file=sys.stderr)
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
    except OSError:
        # Windows 管道模式下 flush 可能报 Invalid argument，忽略即可
        # （数据已 write 到内核缓冲区，Tauri 端 read 时会正常收到）
        pass


def final_event(text: str, seg_samples: np.ndarray | None, embedder: SpeakerEmbedder | None, sample_rate: int = 16000) -> dict:
    """构造一条 final 事件。

    - 始终附带 `speech_duration`：该段**有效语音**时长（秒，裁尾部静音后），
      Rust 端 SCD 据此做时长门槛 / 时长自适应阈值 / 单段新建判定；
    - speaker embedding 模型可用时附带该段音频的 embedding（供 Rust 端余弦匹配）。
    """
    obj = {"type": "final", "text": text}
    if seg_samples is not None:
        samples = trim_trailing_silence(seg_samples)
        obj["speech_duration"] = round(len(samples) / float(sample_rate), 3)
        if embedder is not None and embedder.available:
            emb = embedder.compute(samples)
            if emb:
                obj["embedding"] = emb
    return obj


def run_streaming(stdin, model_dir: str, sample_rate: int, embedding_model_dir: str = ""):
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
        except (ValueError, json.JSONDecodeError):
            emit({"type": "error", "message": f"无法解析配置行: {line!r}"})
            sys.exit(1)

    rec = StreamingRecognizer(model_dir=model_dir, sample_rate=sample_rate)
    embedder = SpeakerEmbedder(model_dir=embedding_model_dir, sample_rate=sample_rate)
    emit(
        {
            "type": "started",
            "streaming": True,
            "model": rec.model_dir.name,
            "sample_rate": rec.sample_rate,
            # T5 SCD：speaker embedding 模型是否可用（Rust 端据此区分 embedding / 单说话人降级）
            "scd_embedding": embedder.available,
        }
    )

    bytes_per_sample = 4  # float32
    buf = b""
    last_partial = ""
    # 自上次 final/reset 以来喂入的音频（用于给该 final 段提取 speaker embedding）。
    # 边界说明：领先沿干净——stream 在上次 final 时已 reset，此处只累积该句音频；
    # 尾沿含端点判定附带的静音，由 trim_trailing_silence 裁掉。chunk 粒度 100ms，
    # 句边界为近似，embedding 用「该句自 reset 以来的全部音频」是合理近似。
    seg_samples: list[np.ndarray] = []

    while True:
        chunk = stdin.read(1600 * bytes_per_sample)  # 100ms @16k
        if not chunk:
            break
        buf += chunk
        if len(buf) >= 1600 * bytes_per_sample:
            n = (len(buf) // (1600 * bytes_per_sample)) * (1600 * bytes_per_sample)
            samples = np.frombuffer(buf[:n], dtype=np.float32).copy()
            buf = buf[n:]
            seg_samples.append(samples)
            partial = rec.feed(samples)
            if partial and partial != last_partial:
                emit({"type": "partial", "text": partial})
                last_partial = partial
            final = rec.maybe_finalize()
            if final:
                seg_audio = np.concatenate(seg_samples) if seg_samples else None
                emit(final_event(final, seg_audio, embedder, sample_rate))
                seg_samples = []
                last_partial = ""

    # stdin EOF → 优雅关闭
    final = rec.finish()
    if final:
        seg_audio = np.concatenate(seg_samples) if seg_samples else None
        emit(final_event(final, seg_audio, embedder, sample_rate))
    emit({"type": "stopped"})


# ---------------------------------------------------------------------------
# 独立运行（--wav）：不依赖 Rust，直接把识别结果打印到 stdout，验证 ASR 链路
# ---------------------------------------------------------------------------


def run_wav(wav_path: str, model_dir: str, sample_rate: int):
    rec = StreamingRecognizer(model_dir=model_dir, sample_rate=sample_rate)
    with wave.open(wav_path, "rb") as w:
        sr = w.getframerate()
        data = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16).astype(
            np.float32
        ) / 32768.0

    print(f"[sherpa_streaming] wav={wav_path} sr={sr} dur={len(data)/sr:.2f}s", file=sys.stderr)
    chunk = int(0.1 * sr)
    last = ""
    for i in range(0, len(data), chunk):
        partial = rec.feed(data[i : i + chunk])
        if partial and partial != last:
            print(f"PARTIAL: {partial}", flush=True)
            last = partial
        final = rec.maybe_finalize()
        if final:
            print(f"FINAL: {final}", flush=True)
            last = ""
    final = rec.finish()
    if final:
        print(f"FINAL: {final}", flush=True)


def main():
    parser = argparse.ArgumentParser(description="sherpa-onnx 流式 ASR sidecar")
    parser.add_argument(
        "--model-dir",
        required=True,
        help="流式模型目录（含 encoder/decoder/joiner/tokens/bpe.model）",
    )
    parser.add_argument("--sample-rate", type=int, default=16000)
    parser.add_argument(
        "--embedding-model-dir",
        default="",
        help="（可选，T5 SCD）说话人 speaker embedding 模型目录（3d-speaker 等，内含 *.onnx）。"
        "提供后每条 final 附带 embedding 字段供 Rust 端说话人余弦匹配；"
        "缺失/加载失败则降级为单说话人",
    )
    parser.add_argument(
        "--wav",
        default="",
        help="独立验证模式：识别该 wav 并打印结果（不需要 Rust stdin）",
    )
    args = parser.parse_args()

    if args.wav:
        run_wav(args.wav, args.model_dir, args.sample_rate)
    else:
        try:
            run_streaming(sys.stdin.buffer, args.model_dir, args.sample_rate, args.embedding_model_dir)
        except Exception as e:  # noqa: BLE001
            emit({"type": "error", "message": f"{type(e).__name__}: {e}"})
            sys.exit(1)


if __name__ == "__main__":
    main()
