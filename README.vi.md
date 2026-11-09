<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="designs/Logo/SVG%20(web)/elisa-lockup-horizontal-on-dark.svg">
    <img alt="Elisa" src="designs/Logo/SVG%20(web)/elisa-lockup-horizontal-on-light.svg" width="360">
  </picture>
</p>

<p align="center"><b>Trợ lý cá nhân sống trên máy Mac của bạn và chạy bằng model của bạn.</b></p>

<p align="center"><a href="README.md">English</a> · <b>Tiếng Việt</b> · <a href="README.ja.md">日本語</a></p>

<p align="center">
  <a href="https://github.com/transybao1393/Elisa/stargazers"><img alt="GitHub stars" src="https://img.shields.io/github/stars/transybao1393/Elisa?style=flat&logo=github"></a>
  <a href="https://github.com/transybao1393/Elisa/releases"><img alt="Latest release" src="https://img.shields.io/github/v/release/transybao1393/Elisa?include_prereleases&sort=semver"></a>
  <a href="https://github.com/transybao1393/Elisa/commits/main"><img alt="Last commit" src="https://img.shields.io/github/last-commit/transybao1393/Elisa"></a>
  <a href="https://github.com/transybao1393/Elisa/issues"><img alt="Open issues" src="https://img.shields.io/github/issues/transybao1393/Elisa"></a>
  <img alt="Platform: macOS 14+" src="https://img.shields.io/badge/platform-macOS%2014%2B-black?logo=apple">
  <img alt="Rust" src="https://img.shields.io/badge/Rust-core-orange?logo=rust">
  <img alt="SwiftUI" src="https://img.shields.io/badge/SwiftUI-app-F05138?logo=swift&logoColor=white">
  <img alt="MCP" src="https://img.shields.io/badge/MCP-plugins-6E56CF">
  <img alt="Local first" src="https://img.shields.io/badge/local--first-private-2EA44F">
  <a href="LICENSE"><img alt="License: Apache 2.0" src="https://img.shields.io/badge/license-Apache%202.0-blue"></a>
</p>


Elisa là một trợ lý trò chuyện nhỏ gọn cho máy Mac. Elisa chạy bằng model do bạn chọn, trong ứng dụng macOS hoặc trên
terminal, cùng với Shade, người bạn nhỏ cho bạn thấy Elisa đang làm gì. Với model chạy trên máy, không có gì rời khỏi
máy Mac của bạn.

## Tính năng

- **Trò chuyện** bằng mọi ngôn ngữ: hỏi đáp, viết lách, lập kế hoạch, kể chuyện cười. Câu hỏi đơn giản được trả lời
  đơn giản, không dùng công cụ.
- **Hợp với model bạn chọn.** Khi khởi động, Elisa nhận biết model làm được gì (xem ảnh, gọi công cụ, suy luận, độ dài
  ngữ cảnh) và tự điều chỉnh. Model không hỗ trợ gọi công cụ vẫn dùng được công cụ qua prompt. Ngữ cảnh nhỏ được Elisa
  tự xử lý.
- **Đổi model giữa cuộc trò chuyện.** Cuộc trò chuyện được giữ nguyên, và model mới trả lời đúng là chính nó.
- **Web.** Tìm kiếm và đọc trang web khi câu hỏi cần thông tin mới. Kết quả được lưu đệm trong một giờ.
- **Tệp và tài liệu.** Đọc và lưu tệp trong thư mục dự án. Hiểu các tệp PDF, bảng tính và Word được đính kèm.
- **Hình ảnh** với model có khả năng xem ảnh, kèm câu hỏi tiếp theo. Model không xem được ảnh sẽ nói rõ thay vì đoán.
- **Ghi nhớ.** Nhớ những gì bạn kể. `/pin` giữ một thông tin, `/notes` cho xem những gì đã giữ, `/forget` xoá hết.
  Cuộc trò chuyện dài được tự động rút gọn (hoặc dùng `/compact`). Tiếp tục cuộc trò chuyện trước khi bạn muốn.
- **Giờ và ngày** lấy từ đồng hồ của máy.
- **Suy luận từng bước** với các công cụ think, steps, calc, chỉ dùng khi câu hỏi cần.
- **Hỗ trợ lập trình**, là một tính năng trong số các tính năng khác: đọc và sửa dự án khi bạn đồng ý, chạy lệnh, và
  chạy thử đoạn code trong sandbox ngoại tuyến (Python, JavaScript, Rust, Go, Bash).
- **Bộ định tuyến ý định (tuỳ chọn).** Một model được lưu trữ trên máy chủ (trả phí) quyết định mỗi tin nhắn cần công cụ
  nào. Không có nó, Elisa tự định tuyến.

## Riêng tư và trong tầm kiểm soát của bạn

- **Model của bạn, bạn chọn.** Ollama, LM Studio, llama.cpp, MLX, hoặc bất kỳ API tương thích OpenAI hay Anthropic.
  Elisa không bao giờ chọn model thay bạn. Model Scorecard cho xem kết quả đo thực tế để bạn chọn.
- **Ưu tiên chạy trên máy.** Với model chạy trên máy, các cuộc trò chuyện ở lại trên máy Mac của bạn.
- **Không có gì xảy ra sau lưng bạn.** Đọc thì tự do. Ghi tệp, chạy lệnh và mọi thứ ngoài thư mục hiện tại đều hỏi
  trước. Quy tắc cho phép / hỏi / từ chối giúp bạn quyết định một lần.
- **An toàn mặc định.** Thông tin bí mật bạn dán vào được ẩn khỏi model trực tuyến và khỏi mọi thứ lưu xuống đĩa, nội
  dung web và tài liệu được coi là không tin cậy, và mọi hành động được ghi vào nhật ký kiểm tra chống chỉnh sửa.
- **Mỗi cuộc trò chuyện bắt đầu mới.** Cuộc trò chuyện mới đi theo những gì bạn hỏi trong đó. Chỉ tiếp tục cuộc trước
  khi bạn chọn.

## Mở rộng

Mọi thứ Elisa dùng được đều cắm vào qua MCP. Các máy chủ có sẵn gồm tìm kiếm web, tài liệu, suy luận và sandbox chạy
code. Plugin đóng gói thêm máy chủ MCP, lệnh gạch chéo và quy tắc quyền từ một thư mục trên máy, và chạy trong container
ngoại tuyến trừ khi cần đến máy Mac của bạn.

## Bắt đầu nhanh

Yêu cầu: macOS 14 trở lên, bộ công cụ Rust và Swift. Colima là tuỳ chọn, cho sandbox chạy code và plugin dạng container.

```sh
# ứng dụng macOS (tự build mọi thứ cần thiết)
app/ElisaApp/run.sh                      # chọn model, rồi bắt đầu trò chuyện

# ứng dụng terminal
cd elisa && cargo build --release        # tệp chạy: elisa/target/release/elisa
elisa/target/release/elisa               # trò chuyện trên terminal
```

## Kho mã

| Đường dẫn | Nội dung |
|---|---|
| [`elisa/`](elisa/) | Elisa (Rust): trò chuyện, công cụ, MCP host và các máy chủ có sẵn, plugin, đánh giá. Tài liệu đầy đủ trong [`elisa/README.md`](elisa/README.md). |
| [`app/ElisaApp/`](app/ElisaApp/) | Ứng dụng macOS (SwiftUI). |
| [`app/ElisaCompanion/`](app/ElisaCompanion/) | Cửa sổ nổi nhỏ của Shade. |
| [`designs/`](designs/) | Biểu tượng ứng dụng và logo. |

## Giấy phép

Apache License 2.0. Copyright 2026 Johnny. Xem [LICENSE](LICENSE), [NOTICE](NOTICE) và
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Giấy phép không cho phép dùng tên, logo Elisa hay nhân vật Shade.
Trọng số model, kể cả bộ định tuyến ý định trên máy chủ, không nằm trong kho mã này và theo giấy phép riêng của chúng.
