# Rust Archiver

跨平台（Windows / Linux / macOS）的壓縮／解壓縮工具，類似 WinRAR。核心與 GUI 皆以 Rust 撰寫（GUI 使用 egui/eframe）。

![screenshot](docs/screenshot.png)

## 功能

- 開啟壓縮檔並列出內容（名稱、大小、壓縮後大小、修改時間），支援搜尋過濾
- 全部解壓縮或只解壓縮選取的檔案／資料夾（Ctrl/Shift 多選）
- 從檔案或資料夾建立壓縮檔，可選擇格式與壓縮等級
- ZIP / 7z 密碼（AES-256）加密與解密，RAR 密碼解壓
- 背景執行緒處理，進度條與取消按鈕
- 拖放：拖入壓縮檔即開啟，拖入一般檔案／資料夾即建立壓縮檔
- 防止 zip-slip 路徑穿越攻擊
- 附帶命令列工具 `rarc`

## 支援格式

| 格式 | 建立 | 解壓 | 密碼 | 壓縮等級 |
|------|:---:|:---:|:---:|:---:|
| ZIP (Deflate) | ✅ | ✅ (Deflate/Deflate64/BZip2/LZMA/XZ/Zstd/PPMd) | ✅ AES-256（解壓也支援 ZipCrypto） | 0–9 |
| 7z (LZMA2) | ✅ | ✅ | ✅ AES-256（含標頭加密） | 0–9 |
| TAR | ✅ | ✅ | — | — |
| TAR.GZ / TGZ | ✅ | ✅ | — | 0–9 |
| TAR.BZ2 / TBZ2 | ✅ | ✅ | — | 1–9 |
| TAR.XZ / TXZ | ✅ | ✅ | — | 0–9 |
| TAR.ZST / TZST | ✅ | ✅ | — | 1–22 |
| GZ / BZ2 / XZ / ZST（單一檔案） | ✅ | ✅ | — | 同上 |
| RAR (v4 / v5) | ❌ | ✅ | ✅ | — |

> **為什麼不能建立 RAR？** RAR 壓縮演算法為 RARLAB 的專有格式，官方 UnRAR 原始碼授權只允許解壓縮，明文禁止用來重建 RAR 壓縮器，因此本程式只能解壓 RAR。

## 下載

到 [Releases](https://github.com/stevenke1981/rust-archiver/releases) 下載對應平台的檔案：

- Windows：`rust-archiver-windows-x64.zip`
- Linux：`rust-archiver-linux-x64.tar.gz`（需 GTK3 執行環境供檔案對話框使用）
- macOS：`rust-archiver-macos-arm64.tar.gz`（未簽章，首次開啟需在「系統設定 → 隱私權與安全性」允許）

## 從原始碼建置

```bash
# Linux 需先安裝：
sudo apt install build-essential pkg-config libgtk-3-dev libxkbcommon-dev libgl1-mesa-dev libwayland-dev
cargo build --release
cargo test
./target/release/rust-archiver [壓縮檔]
```

## 命令列 `rarc`

```bash
rarc l archive.7z -p 密碼          # 列出內容
rarc x archive.rar -o 輸出資料夾     # 解壓（可在後面列出指定項目）
rarc c out.tar.zst 資料夾 檔案 -l 19  # 建立（格式依副檔名決定）
```

## 專案結構

- `core/` — `archiver-core` 函式庫：格式偵測、列出、解壓、建立
- `gui/` — `rust-archiver` GUI 程式與 `rarc` CLI

## 已知限制

- RAR 僅能解壓（授權限制）；RAR 解壓使用 `unrar` crate（內含 C++ UnRAR 原始碼，建置時需 C++ 編譯器）
- 不支援分卷壓縮檔的建立；7z 分卷與自解壓檔（SFX）尚未支援
- 不支援直接在壓縮檔內新增／刪除檔案（需重新建立）
- 單一檔案格式（.gz/.bz2/.xz/.zst）一次只能壓縮一個檔案，多檔請用 TAR.* 格式

## License

MIT

---

## English summary

**Rust Archiver** is a WinRAR-like, cross-platform (Windows/Linux/macOS) archive manager with a Rust core library and an egui GUI.
It can open and list archives, extract all or selected entries, and create ZIP, 7z, TAR, TAR.GZ/BZ2/XZ/ZST and single-file GZ/BZ2/XZ/ZST archives with selectable compression level.
ZIP and 7z support AES-256 passwords; RAR (v4/v5) is **extract-only** because the RAR compression format is proprietary and the UnRAR license forbids re-creating a RAR compressor.
Work runs on a background thread with a progress bar and cancel button; drag-and-drop opens archives or adds files to a new archive.
A small CLI (`rarc`) is included. Prebuilt binaries are on the Releases page. Licensed under MIT.
