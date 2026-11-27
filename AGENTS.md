# AGENTS.md — isor-studio

## Tổng quan dự án
`isor-studio` là ứng dụng DAW local-first viết bằng Rust (GPUI Desktop + Local Rust Backend Daemon) tích hợp CLI Agent Harness (`codex` / `claude`) để tạo sinh và chỉnh sửa các nhạc cụ có cấu trúc (YAML/JSON).

Trước khi thiết kế hoặc viết code, BẮT BUỘC đọc:
- `docs/PRODUCT_BRIEF.md`
- `docs/ARCHITECTURE.md`
- `docs/ROADMAP.md`

## Quy tắc kiến trúc cốt lõi
1. **Contract-first:** Mọi thay đổi dữ liệu Project IR hoặc giao thức JSON-RPC phải cập nhật trong `crates/shared` trước khi triển khai song song ở `crates/backend` và `crates/app`.
2. **Audio Real-time Safety (`crates/audio`):** Tuyệt đối không cấp phát bộ nhớ (heap allocation), không khóa mutex chặn (blocking lock) và không làm I/O trong audio callback của `cpal`.
3. **GPUI Executor Safety (`crates/app`):** Không bao giờ block foreground executor; mọi tác vụ mạng/IPC/file đều phải chạy trên `cx.background_executor()`.
4. **Hot-reload an toàn:** File watcher khi đọc file YAML/JSON bị lỗi cú pháp (do agent đang ghi dở) phải báo lỗi validation gọn gàng, giữ nguyên audio graph cũ, không panic.
5. **Chất lượng code:** Không dùng `unwrap()` / `expect()` trong production code; mọi PR phải pass `cargo clippy --workspace --all-targets -- -D warnings` và `cargo nextest run`.