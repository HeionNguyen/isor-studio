# Roadmap: isor-studio

Roadmap được chia theo **Vertical Slices** (lát cắt dọc chạy thông suốt từ `shared` → `backend`/`audio` → `gpui` → test) để nhóm Orca Bots (`@lead`, `@backend`, `@gpui`, `@pr-review`) phối hợp song song trên các Git worktrees độc lập.

---

## Phase 0: Walking Skeleton (Nền móng & Luồng chạy thông suốt)
*Mục tiêu: Toàn bộ ống dẫn GPUI ↔ Backend Daemon ↔ Audio Engine ↔ Project Folder chạy thật với 1 bài nhạc mẫu tối giản.*

| Slice | Nội dung chính | Bot phụ trách | Acceptance Criteria |
| :--- | :--- | :--- | :--- |
| **0.1** | Khởi tạo Cargo workspace (`shared`, `audio`, `backend`, `app`), `orca.yaml`, `AGENTS.md`, cấu hình CI (`clippy` + `nextest`). | `@lead` | `cargo check --workspace` & `cargo nextest run` xanh. |
| **0.2** | Định nghĩa Project IR v0 (`Project`, `Track`, `NoteEvent`) & JSON-RPC protocol tối giản (`Ping`, `LoadProject`, `Play`, `Stop`, `GetState`) trong `crates/shared`. | `@lead` | Serialize/deserialize YAML mẫu pass unit test. |
| **0.3** | Backend Daemon cơ bản + `crates/audio` phát được 1 âm thanh synth/SF2 đơn giản từ `project.yaml` mẫu + File Watcher hot-reload. | `@backend` | Sửa file YAML bằng tay → backend tự reload không crash; test audio render offline tạo ra buffer khác 0. |
| **0.4** | Cửa sổ GPUI kết nối Local WebSocket tới Backend, hiển thị tên bài + BPM + nút Play/Stop điều khiển được playback thật. | `@gpui` | `#[gpui::test]` xác nhận state chuyển đổi khi nhận event từ mock backend; chạy tay nghe được tiếng. |

---

## Phase 1: Core MVP — "Suno-meets-DAW Lite"
*Mục tiêu: Người dùng không chuyên có thể prompt AI tạo một bài nhạc đa nhạc cụ, nghe ngay, sửa trực quan hoặc nhờ AI sửa tiếp, Undo/Compare và Export WAV/MIDI.*

| Slice | Nội dung chính | Phân công song song |
| :--- | :--- | :--- |
| **1.1 – Multi-Track Playback & Built-in SF2/Synth** | Hỗ trợ đa track (Drums, Bass, Piano, Lead...), Mixer (Volume/Pan/Mute/Solo), sắp xếp theo Sections (Intro/Verse/Chorus), nhúng sẵn 1 bộ SoundFont (SF2) nhẹ + Synth cơ bản. | `@lead` (IR v1) → `@backend` (Audio mixer + sequencer) + `@gpui` (Timeline & Track Mixer UI) |
| **1.2 – CLI Agent Harness (`codex` / `claude`)** | Backend tự sinh `AGENT_SCHEMA.md` + lệnh `isor validate`, bọc CLI agent chạy trên thư mục project, stream log/tiến độ và cho phép truyền context vùng chọn (`selected_track`, `selected_section`). | `@lead` (RPC protocol) → `@backend` (CLI runner + streaming) + `@gpui` (Agent Chat panel + trạng thái đang tạo) |
| **1.3 – Auto Git Snapshot (Undo & A/B Compare)** | Tự commit snapshot trước/sau mỗi lượt AI sửa; API liệt kê lịch sử phiên bản, khôi phục (rollback) hoặc chuyển nhanh A/B giữa 2 phiên bản đang phát. | `@backend` (Git snapshot engine) + `@gpui` (Version history list + nút Undo / A-B Compare) |
| **1.4 – Simple Instrument Editor (Piano Roll & Presets)** | Click vào 1 block trên Timeline mở Piano Roll tối giản (thêm/xóa/kéo nốt cơ bản) + chọn Preset nhạc cụ/chỉnh tham số cơ bản → ghi ngược xuống file YAML và sync audio. | `@lead` (Edit commands) → `@backend` (Patch YAML + validation) + `@gpui` (Piano Roll canvas + Preset controls) |
| **1.5 – Export WAV & MIDI** | Offline bounce toàn bộ bài nhạc ra file `.wav` (16/24-bit) và xuất `.mid` chuẩn nhiều track. | `@backend` (Offline WAV/MIDI exporter) + `@gpui` (Export dialog + progress bar) |

---

## Phase 2: Mở rộng chuyên sâu & Hệ sinh thái Plugin
*Mục tiêu: Nâng chất lượng âm thanh ngang DAW thực thụ và mở rộng nền tảng.*

- **2.1 – External VST3 / CLAP Hosting:** Triển khai `ExternalPluginInstrument` bám theo trait `InstrumentBackend` đã thiết kế từ Phase 1; quét plugin trên máy, nạp preset, điều khiển tham số qua UI (và cho phép LLM chỉnh tham số plugin trong YAML).
- **2.2 – Export nâng cao:** Xuất tách từng track (**Stems WAV**) và **MP3**.
- **2.3 – Đa nền tảng & UX nâng cao:** Hỗ trợ đầy đủ macOS, Linux, Windows; phím tắt DAW chuẩn, loop region, automation volume/filter cơ bản.