#!/usr/bin/env node
/**
 * 回归检查：真实 SCD 按音色分人（修复「多人说话全归到一个说话人」）。
 *
 * 链路（与 Rust 壳 `src-tauri/src/asr.rs` 一致）：
 *   sherpa_streaming.py（流式，加载 speaker embedding 模型）
 *     → NDJSON final（含 512 维 embedding）
 *     → examples/scd_emit（读 NDJSON，调用 engine::Scd 余弦匹配）
 *     → 输出每条 final 的 speaker_id
 *
 * 断言（用户报告的 bug：多人说话全归说话人 1）：
 *   1. sidecar `started.scd_embedding == true`（模型已配置并加载）；
 *   2. 每条 final 携带 512 维 embedding；
 *   3. 不同 wav（不同说话人）被分到不同 speaker，且同一个 wav 内的
 *      final 保持同一 speaker（SCD 稳定性）。
 *
 * 退出码：0 = 绿；1 = 红。
 * 用法：node scripts/check-scd-embedding.mjs
 * （可设 SHERPA_TEST_WAVS="w1.wav,w2.wav" 覆盖测试音频）
 */

import { execFileSync, spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const root = fileURLToPath(new URL("..", import.meta.url));
const python = path.join(root, "src-tauri/.venv/bin/python3");
const sidecar = path.join(root, "src-tauri/sherpa_streaming.py");
const example = path.join(root, "target/debug/examples/scd_emit");
const manifest = path.join(root, "src-tauri/Cargo.toml");

const modelDir = path.join(
  root,
  "src-tauri/asr-models/sherpa-onnx-x-asr-960ms-streaming-zipformer-transducer-zh-en-punct-int8-2026-06-05",
);
const embeddingDir = path.join(root, "src-tauri/asr-models/sherpa-onnx-3dspeaker-eres2net-base");
const defaultWavs = [0, 1, 2, 3].map(
  (i) => path.join(modelDir, "test_wavs", `${i}.wav`),
);
const wavs = process.env.SHERPA_TEST_WAVS
  ? process.env.SHERPA_TEST_WAVS.split(",").map((p) => path.resolve(p))
  : defaultWavs.map((p) => path.resolve(p));

const checks = [];
function check(name, pass, detail) {
  checks.push({ name, pass, detail });
}

// 确保 scd_emit 已编译（示例二进制）。
if (!readFileExists(example)) {
  execFileSync("cargo", ["build", "--manifest-path", manifest, "--example", "scd_emit"], {
    stdio: "inherit",
  });
}

/**
 * 以流式模式跑 sidecar 处理一段 wav，收集 NDJSON 事件。
 * 与 Rust 端 `SherpaAsr::spawn` 相同：首行 config，随后喂 16kHz float32 PCM。
 */
function runSidecar(wav) {
  const frame = readWav(wav);
  const configLine = Buffer.from(
    `${JSON.stringify({ type: "config", sample_rate: frame.sampleRate })}\n`,
    "utf8",
  );
  const res = spawnSync(
    python,
    [
      sidecar,
      "--model-dir",
      modelDir,
      "--embedding-model-dir",
      embeddingDir,
      "--sample-rate",
      String(frame.sampleRate),
    ],
    { input: Buffer.concat([configLine, frame.pcm]), maxBuffer: 64 * 1024 * 1024 },
  );
  if (res.error) {
    throw res.error;
  }
  if (res.status !== 0) {
    throw new Error(`sidecar 退出码 ${res.status}: ${res.stderr?.toString() || ""}`);
  }
  const lines = res.stdout
    .toString("utf8")
    .split(/\r?\n/)
    .filter((l) => l.trim());
  return lines.map((l) => JSON.parse(l));
}

function readWav(wav) {
  const buf = readFileSync(wav);
  const sampleRate = buf.readUInt32LE(24);
  const byteRate = buf.readUInt32LE(28);
  const bitsPerSample = buf.readUInt16LE(34);
  // 找 data 块，取 PCM 字节。
  let dataStart = -1;
  let dataLen = 0;
  let cursor = 12;
  while (cursor + 8 <= buf.length) {
    const id = buf.toString("ascii", cursor, cursor + 4);
    const size = buf.readUInt32LE(cursor + 4);
    if (id === "data") {
      dataStart = cursor + 8;
      dataLen = size;
      break;
    }
    cursor += 8 + size + (size % 2);
  }
  if (dataStart < 0) {
    throw new Error(`无法定位 data 块: ${wav}`);
  }
  const raw = buf.subarray(dataStart, dataStart + dataLen);
  const samples = new Float32Array(raw.length / 2);
  for (let i = 0; i < samples.length; i += 1) {
    const v = raw.readInt16LE(i * 2) / 32768.0;
    samples[i] = v;
  }
  return { sampleRate, pcm: Buffer.from(samples.buffer, samples.byteOffset, samples.byteLength) };
}

function readFileExists(p) {
  try {
    return readFileSync(p).length > 0;
  } catch {
    return false;
  }
}

// 每段 wav 的 final（含 embedding），全部喂进同一个 scd_emit（按时间顺序）。
const byWav = [];
let startedOk = true;
let embeddingOk = true;
for (const wav of wavs) {
  const events = runSidecar(wav);
  const started = events.find((e) => e.type === "started");
  const finals = events.filter((e) => e.type === "final");
  if (!started?.scd_embedding) startedOk = false;
  if (!finals.length) throw new Error(`wav 无 final 输出: ${wav}`);
  for (const f of finals) {
    if (!Array.isArray(f.embedding) || f.embedding.length !== 512) embeddingOk = false;
  }
  byWav.push({ wav, finals });
}
check("sidecar 确认加载 speaker embedding 模型", startedOk);
check("每条 final 携带 512 维 embedding", embeddingOk);

const scdInput = Buffer.concat(
  byWav.flatMap(({ finals }) => finals.map((f) => Buffer.from(`${JSON.stringify(f)}\n`, "utf8"))),
);
const scdRes = spawnSync(example, [], { input: scdInput, maxBuffer: 64 * 1024 * 1024 });
if (scdRes.status !== 0) {
  throw new Error(`scd_emit 失败: ${scdRes.stderr?.toString() || ""}`);
}
const rows = scdRes.stdout
  .toString("utf8")
  .split(/\r?\n/)
  .filter((l) => l.trim())
  .map((l) => JSON.parse(l));

// 逐 wav 分组：按 scd_emit 的行序切回各 wav。
let cursor = 0;
const perWavSpeakers = byWav.map(({ finals }) => {
  const ids = rows.slice(cursor, cursor + finals.length).map((r) => r.speaker_id);
  cursor += finals.length;
  return ids;
});
const stableWithinWav = perWavSpeakers.every((ids) => new Set(ids).size === 1);
const distinctAcrossWavs = new Set(perWavSpeakers.map((ids) => ids[0])).size >= 2;
check("同一 wav 内 final 归属同一说话人", stableWithinWav);
check("不同 wav（不同说话人）分到不同说话人", distinctAcrossWavs);

for (let i = 0; i < byWav.length; i += 1) {
  console.log(
    `  [scd] ${path.basename(byWav[i].wav)} -> speaker ${perWavSpeakers[i].join(",")}`,
  );
}
for (const c of checks) {
  console.log(`[scd-embedding] ${c.name}: ${c.pass ? "PASS" : "FAIL"}${c.pass ? "" : ` (${c.detail ?? ""})`}`);
}

if (checks.every((c) => c.pass)) {
  console.log("[scd-embedding] PASS：真实 SCD 按音色分人生效");
  process.exit(0);
}
console.error("[scd-embedding] FAIL：SCD 未正确按音色分组");
process.exit(1);
