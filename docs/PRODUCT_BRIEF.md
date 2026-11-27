# Product Brief: isor-studio

## 1. Tầm nhìn & Bài toán
`isor-studio` là ứng dụng desktop chạy cục bộ (local-first) kết hợp giữa **DAW tối giản** và **AI Agent Harness** (lấy cảm hứng từ Suno AI + Codex/Claude Code).

Thay vì tạo ra một file audio hoàn chỉnh "chết" khó can thiệp như Suno, `isor-studio` cho phép người dùng mô tả ý tưởng bằng ngôn ngữ tự nhiên để AI tạo sinh **các track nhạc cụ (instruments) có cấu trúc trên đĩa (YAML/JSON)**. Sau đó, cả người dùng (qua giao diện trực quan) và AI (qua chỉnh sửa file có cấu trúc) có thể liên tục lặp lại, tinh chỉnh từng đoạn, từng nốt hoặc từng nhạc cụ.

## 2. Đối tượng sử dụng
- Người sản xuất âm nhạc không chuyên / indie creator / developer thích làm nhạc.
- Muốn bắt đầu nhanh bằng prompt nhưng vẫn kiểm soát được cấu trúc bài nhạc, phối khí, âm lượng, nốt nhạc và xuất ra WAV/MIDI để dùng tiếp.

## 3. Luồng nghiệp vụ cốt lõi (Core Workflows)
1. **Tạo bài mới từ Prompt:** Người dùng nhập mô tả (thể loại, mood, tempo, cấu trúc Intro/Verse/Chorus) → AI Agent tạo cấu trúc project & các track nhạc cụ → Backend tự hot-reload và phát nhạc ngay lập tức.
2. **Chỉnh sửa cộng tác với AI theo ngữ cảnh:** Người dùng chọn một track (vd: `drums`) hoặc một section (vd: `Chorus`) trên Timeline và yêu cầu AI chỉnh lại ("làm bass groove hơn, giữ nguyên hợp âm") → AI chỉ sửa phần liên quan → tự động tạo Git snapshot để người dùng **Undo / A-B Compare** dễ dàng.
3. **Chỉnh sửa trực quan (DAW Lite):** Người dùng tự điều chỉnh Volume/Mute/Solo trên Timeline, kéo thả/sửa nốt trên Piano Roll tối giản, đổi preset nhạc cụ — mọi thay đổi được ghi ngược lại cấu trúc project để AI hiểu ở lượt prompt tiếp theo.
4. **Xuất bản (Export):** Render bài nhạc ra file **WAV** (offline bounce) và **MIDI (`.mid`)**.

## 4. Phạm vi MVP (Phase 1) vs. Non-goals
- **In-scope (MVP):**
  - Chạy local mượt trên 1 hệ điều hành chính (macOS).
  - Project lưu dạng thư mục YAML/JSON + Git auto-snapshot.
  - Built-in Rust Audio Engine (SoundFont `.sf2` + Synth cơ bản) nghe được ngay không cần cài plugin ngoài.
  - Bọc CLI Agent bên ngoài (`codex` / `claude`) thao tác trực tiếp lên thư mục project + hot-reload.
  - Giao diện GPUI 3 vùng: Agent Chat & Version/Undo, Arrangement Timeline, Piano Roll & Preset Editor tối giản.
  - Export WAV và MIDI.
- **Non-goals ở MVP (đẩy sang Phase 2+):**
  - Điều khiển VST3/CLAP plugins bên ngoài (kiến trúc chuẩn bị sẵn trait từ Phase 1, triển khai ở Phase 2).
  - Thu âm giọng hát / audio recording trực tiếp từ micro.
  - Export đa track Stems, MP3 và hỗ trợ đầy đủ 3 OS (macOS/Linux/Windows).
  - Cộng tác cloud / tài khoản online.