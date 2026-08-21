<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="brand/logo-horizontal-dark.png">
    <img alt="TalkSee · Make conversation visible" src="brand/logo-horizontal-light.png" width="440">
  </picture>
</p>

# TalkSee — Real-time captioning for the hearing-impaired

> **Make conversation visible.** TalkSee (语见) is a cross-platform desktop app (macOS / Windows) that provides **real-time captions** for deaf and hard-of-hearing people during face-to-face conversations.
> It transcribes what people around you say, displays each speaker as a colored bubble with a random avatar, uses an LLM to smooth the raw speech into readable prose, and generates a structured meeting summary when a session ends. **Officially released (v0.4.0), built entirely on Tauri 2 + Rust engine + Python sherpa-onnx.**

## Navigation

- 中文文档：[README.md](README.md) · [更新日志](CHANGELOG.md)
- English: this file — [README.en.md](README.en.md) · [Changelog](CHANGELOG.md)

- Version: **v0.4.0**
- CI: [![CI](https://github.com/smallmj/talksee/actions/workflows/ci.yml/badge.svg)](https://github.com/smallmj/talksee/actions/workflows/ci.yml) · [![Release](https://github.com/smallmj/talksee/actions/workflows/release.yml/badge.svg)](https://github.com/smallmj/talksee/actions/workflows/release.yml)
- License: [MIT](LICENSE) · Spec: [Issue #1](https://github.com/smallmj/talksee/issues/1) (closed)
- Implementation notes: `docs/T*-implementation-summary.md` · Architecture decisions: `docs/adr/`

---

## Why we built this

Hundreds of millions of people worldwide live with some degree of hearing loss (the WHO estimates ~430 million with disabling loss). For them, **face-to-face conversations of 3–5 people are among the hardest situations**: they can't hear what's being said, can't keep up with the pace, and when it's their turn they worry about "jumping in without having heard everything." Lip-reading, sign language, and asking someone to relay all fall short of real, fast, simultaneous conversation — a glance, an interruption, a quiet aside can shut a hard-of-hearing person out.

Technology has finally made this solvable: **local streaming speech recognition** (sherpa-onnx) is fast, accurate, and cheap enough; **large language models** can turn colloquial transcripts into readable prose; **per-speaker display** makes the structure of a conversation instantly clear. This project combines these into a tool that "listens for the deaf and makes conversation visible."

### Three principles we hold

1. **Local-first, privacy by default**: Conversation is private. Everything runs locally by default (offline ASR, local models); audio and text never leave the machine. Cloud ASR / LLM only go online when you explicitly enable and configure them.
2. **Real-time and readable**: Captions appear "as you speak," grouped into color-coded bubbles per speaker, showing the LLM-cleaned prose by default with a one-click toggle to the raw transcript — fast enough to follow, comfortable enough to read.
3. **Open, customizable, affordable**: Open source means anyone who needs it can use, review, and improve it. Local models run free — no per-conversation cost — and that also makes privacy and safety auditable.

### Vision

Make every conversation **visible, followable, and participable** for the deaf and hard-of-hearing. If you or someone you know needs this, welcome — use it, give feedback, contribute, and share it with those who need it more.

---

## Features

### Real-time recognition (local-first)
- **Streaming ASR**: local sherpa-onnx streaming recognition, transcribing word-by-word (primarily Mandarin + mixed Chinese/English).
  - Default model: **streaming paraformer bilingual zh-en** (real-time, ~1s chunk latency, better English & noise robustness).
  - Optional **SenseVoice high-accuracy mode** (VAD-segmented per-utterance, ITN + punctuation, e.g. `五千八百块 → 5800`; non-streaming, ~2–4s per sentence).
- **Local-first**: offline recognition by default; audio and text never leave the machine.
- **Cloud switchable**: Deepgram-compatible streaming WebSocket; one toggle in settings.

### Speaker change detection (SCD)
- **VAD-first segmentation** (Silero): speech segments drive the transcript/embedding/bubble boundary — a new speaker's onset is no longer spliced onto the previous speaker's final.
- Head / tail / whole-segment **multi-window embedding voting** + in-sentence **split-and-reassign** self-healing; **background FastClustering backfill** corrects misattributions.
- Per-speaker **stable color**, random avatar; manual rename ("Speaker 2" → "Zhang San") and avatar swap/shuffle.
- Short-utterance & noise guards: no phantom speakers, colors don't shift in long sessions.

### Dual-track cleanup (LLM)
- OpenAI-compatible endpoint (Base URL + API Key + model name), **SSE streaming** output, exponential-backoff retry ×3.
- Configurable interval (5s / 10s) removes filler, corrects errors, adds punctuation.
- **Cleaned text shown by default**, one-click toggle to raw; **diff highlighting** for edits; on failure the raw text is kept (no content loss).

### Meeting minutes
- After stopping recognition, segments are auto-batched (~500 chars/batch + rolling context) and sent to the LLM, then summarized into **【Key Points】【Action Items】【To-dos】**.

### Session history & export
- Sessions auto-save locally and survive restarts; a history list lets you reopen and review.
- Export **Markdown / TXT / SRT** (timestamped per line).

### Display & interaction
- Always-on-top focus mode (always on top + large font; exit via Esc / ✕).
- Light/dark theme follows system or overrides manually; custom font size / family / text color.
- **Bubble list scrolls internally**; the toolbar/status bar stays fixed; latest content is auto-scrolled-to when you're near the bottom, pauses when you scroll up, with a **"Back to latest"** button.

### Desktop integration & settings
- **Tray resident** + **global hotkeys** (⌘/Ctrl+Shift+L/H/S/T) + single-instance (relaunch summons the main window).
- First-run wizard: runtime check + ASR / speaker-model download (optional China mirror); returns to wizard on startup until complete.
- Settings center: General / Models / LLM Cleanup / Display / Shortcuts / History / About — tabbed groups + hints + persistence + instant effect.
- **In-app auto-update**: on Windows, check in Settings → About, download & install (restart after confirmation); on macOS (unsigned builds) a new version opens the GitHub Releases page for manual download.

---

## Architecture

```
┌──────────────────────────────────────────────────────────┐
│ src/   React 18 frontend (thin renderer)                 │
│   dual-track bubble stream · speaker badges · minutes     │
│   history · settings · first-run wizard                   │
└───────────────▲──────────────────────────────────────────┘
                │  Tauri IPC (invoke + engine://event stream)
┌───────────────┴──────────────────────────────────────────┐
│ src-tauri/   Tauri 2 shell (glue)                        │
│   audio capture(cpal) · local/cloud ASR drivers          │
│   LLM client(ureq+SSE) · tray/hotkey/single-instance     │
│   session persistence · export · first-run & model dl    │
│   ─────────────────────────────────────────────────────  │
│   sherpa_streaming.py  Python sidecar (stdin/stdout      │
│   NDJSON, 16kHz PCM streaming + speaker embedding)       │
└───────────────▲──────────────────────────────────────────┘
                │  AsrPort / EmbeddingPort / LlmPort (injected, no Tauri dep)
┌───────────────┴──────────────────────────────────────────┐
│ engine/   Rust core (the single test seam)               │
│   segment mgmt · SCD(cosine match) · LLM cleanup pipeline│
│   (debounce+rhythm+editId) · minutes batching · events    │
└──────────────────────────────────────────────────────────┘
```

- **engine** (`engine/`): an independent Rust crate holding all business logic, no Tauri dependency. External capabilities (audio capture, ASR, LLM, embedding) are injected via `AsrPort` / `EmbeddingPort` / `LlmPort`; it exposes a unified event stream. **This is the app's single test seam** (synthetic input → assert event stream, no real audio/network/WebView).
- **Tauri shell** (`src-tauri/`): a thin glue layer — audio capture (cpal), local/cloud ASR drivers, LLM client (ureq blocking + SSE parser), tray/hotkey/single-instance, session persistence & export, first-run & model download.
- **Python sidecar** (`src-tauri/sherpa_streaming.py`): sherpa-onnx streaming ASR communicating over stdin/stdout NDJSON; runnable standalone to verify the ASR path.
- **React frontend** (`src/`): deliberately thin — consumes the event stream only.

Domain vocabulary in [CONTEXT.md](CONTEXT.md); key decisions in `docs/adr/` (0001 Tauri cross-platform, 0002 local streaming ASR + self-built SCD, 0003 dual-track LLM cleanup, 0004 switchable cloud ASR, 0005 VAD-first segmentation, 0006 paraformer default ASR, 0007 updater via GitHub Releases).

---

## Quick start (development)

Prereqs: Node.js ≥ 18 (pnpm), Rust toolchain (for Tauri 2), Python 3.

```bash
pnpm install            # frontend deps
pnpm run setup:runtime  # create src-tauri/.venv and install sherpa-onnx + numpy (idempotent)
pnpm tauri dev          # start dev mode
```

The first launch shows the **initialization wizard**: check environment → download the ASR model (optional hf-mirror China mirror, resumable) → download the speaker model (skippable; if skipped SCD falls back to single-speaker) → enter the main UI.

> Missing / switching models: Settings → Models to download or switch; cloud ASR and LLM cleanup configured on their own tabs (OpenAI-compatible: Base URL + API Key + model name).

## Download & install (release)

Download the installer for your platform from [GitHub Releases](https://github.com/smallmj/talksee/releases):

| Platform | Installer | Notes |
|----------|-----------|-------|
| macOS (Apple Silicon) | `TalkSee_*_aarch64.dmg` | open the DMG, drag TalkSee into Applications |
| macOS (Intel) | `TalkSee_*_x64.dmg` | same |
| Windows | `TalkSee_*_x64-setup.exe` | NSIS one-click install |

> **Unsigned build**: code signing/notarization is not yet enabled, so first installs may show a security prompt — see below.

**macOS**: if you see "cannot verify the developer," right-click TalkSee → Open → Confirm; or after dragging into Applications run:

```bash
xattr -dr com.apple.quarantine "/Applications/TalkSee.app"
```

**Windows**: if SmartScreen says "Windows protected your PC," click **More info → Run anyway**.

The first launch runs the initialization wizard: detects the environment and downloads recognition models (optional China mirror); afterwards recognition is fully local and audio never leaves the machine.

## Building

```bash
pnpm tauri build                          # local verify: runs pnpm build + package:runtime automatically
TALKSEE_STANDALONE=1 pnpm tauri build     # distribution: bundles self-contained Python (python-build-standalone), portable across machines
```

Artifacts land in `target/release/bundle/` (macOS DMG: `bundle/dmg/`; Windows NSIS: `bundle/nsis/`). The default mode copies your local `src-tauri/.venv` and is for local verification only; **always use `TALKSEE_STANDALONE=1` for official releases** (the GitHub Actions release workflow does this by default). The packaged runtime ships the Python runtime inside; first run only does health checks and model download.

## Releasing a new version

1. Sync the version number across `src-tauri/tauri.conf.json` / `Cargo.toml` / `package.json`.
2. Tag and push; CI auto-builds macOS/Windows installers and creates a **draft** Release:

   ```bash
   git tag v0.4.0 && git push origin v0.4.0
   ```

3. Review the draft on GitHub Releases and publish it when ready.

Workflow [`.github/workflows/release.yml`](.github/workflows/release.yml): macOS (arm64/x64) → DMG, Windows (x64) → NSIS; triggered by tag, also runnable manually (Actions → Release → Run workflow).

## Testing

```bash
cargo test --workspace          # engine unit tests + shell tests (single test seam)
pnpm build                      # tsc type-check + Vite build
pnpm check:dual-track           # dual-track display regression
pnpm check:llm-nonblocking      # LLM non-blocking cleanup regression
pnpm check:focus-exit           # focus-mode exit regression
pnpm check:scd-embedding        # SCD embedding regression
```

## Project layout

```
src/                  React frontend (components, dual-track display, settings, session history, first-run wizard)
engine/               Rust core (business logic + test seam)
src-tauri/            Tauri 2 shell + Python sidecar (sherpa_streaming.py)
scripts/              setup-runtime / package-runtime / regression-check scripts
docs/                 ADR, ticket index, per-ticket implementation notes, research reports
```

## Known limitations (current)

- Installers are not code-signed/notarized (macOS Gatekeeper & Windows SmartScreen may prompt; see Download & install); Windows supports in-app auto-update, macOS (unsigned) upgrades by opening the Releases page.
- Only face-to-face microphone capture; no system/online-meeting audio; no bubble playback.
- SCD only detects speakers + manual naming; no full auto diarization / cross-day identity persistence (since v0.4: VAD-segmented turns + head/tail multi-window voting + background cluster backfill; short utterances / interruptions still hit the model's limit).
- Avatar gender is not auto-derived from voice; no pause/resume during recognition (see Issue #1 Out of Scope).
- Cloud ASR is a Deepgram-compatible protocol; other vendors need shell-side protocol adapters.
