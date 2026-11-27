# Architecture: isor-studio

## 1. Cấu trúc Cargo Workspace
- `crates/shared`:
  - Data model / Project IR (`Project`, `Section`, `Track`, `Pattern`, `NoteEvent`, `InstrumentSpec`, `MixerState`) chuẩn hóa bằng `serde` (YAML/JSON).
  - Protocol giao tiếp giữa GPUI và Backend (JSON-RPC commands & server events).
  - Validator (kiểm tra schema, dải MIDI 0–127, nhịp/BPM, tham chiếu track hợp lệ).
- `crates/audio`:
  - Real-time Audio Engine (`cpal` output stream + lock-free ring buffer cho lệnh điều khiển).
  - Trait `InstrumentBackend` trừu tượng hóa nguồn âm thanh:
    - Phase 1: `SoundFontInstrument` (`rustysynth`) + `BasicSynthInstrument` (`fundsp`).
    - Phase 2: `ExternalPluginInstrument` (VST3/CLAP host).
  - Offline Renderer cho Export WAV và bộ chuyển đổi MIDI (`.mid`).
- `crates/backend`:
  - Local daemon lắng nghe WebSocket/IPC cục bộ (localhost).
  - **CLI Agent Harness:** Spawn và quản lý phiên `codex` / `claude` CLI chạy trong thư mục project; truyền ngữ cảnh vùng chọn (selected track/section) và stream tiến độ về GPUI.
  - **File Watcher (`notify`):** Theo dõi thay đổi file trên đĩa → validate → hot-reload vào Audio Engine (không làm crash tiếng khi file đang được ghi dở) → broadcast trạng thái mới sang GPUI.
  - **Version Manager:** Tự động tạo Git commit snapshot trước/sau mỗi lượt AI chỉnh sửa để hỗ trợ Undo và A/B Compare 1-click.
- `crates/cli` (tùy chọn nhỏ hoặc subcommand của `backend`):
  - Cung cấp lệnh `isor validate` để CLI Agent tự kiểm tra file YAML/JSON hợp lệ trước khi kết thúc lượt.
- `crates/app`:
  - Ứng dụng desktop viết bằng **GPUI**.
  - Tuyệt đối không chạy I/O hay xử lý nặng trên foreground executor; toàn bộ giao tiếp Backend chạy trên `cx.background_executor()`.

## 2. Cấu trúc thư mục một Project bài nhạc (trên đĩa)
```text
my-song/
├── .git/                 # Auto-snapshot mỗi lượt AI sửa (ẩn với người dùng không chuyên)
├── AGENT_SCHEMA.md       # Hướng dẫn schema & quy tắc nhạc lý tự sinh để Codex/Claude đọc
├── project.yaml          # Metadata: title, bpm, time_signature, key, sections (Intro/Verse/Chorus)
└── tracks/
    ├── drums.yaml        # Instrument spec (SF2/Synth/VST), mixer (vol/pan/mute), patterns & notes
    ├── bass.yaml
    └── piano.yaml
```