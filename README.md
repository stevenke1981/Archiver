# Archiver

Archiver 是跨平台（Windows / Linux / macOS）的壓縮與解壓縮工具，主視窗仿 WinRAR 的版面。核心函式庫 `archiver-core` 與命令列工具 `rarc` 以 Rust 撰寫；圖形介面 `archiver` 使用 [AcmeUI-Native](https://github.com/stevenke1981/AcmeUI-Native) 繪製，GPU（wgpu）優先，沒有可用 GPU 時自動改用 CPU 繪製。

![Archiver 主視窗](docs/screenshot.png)

## 快速開始

### 下載與安裝

到 [Releases](https://github.com/stevenke1981/Archiver/releases) 下載對應平台的檔案：

- **Windows**：`archiver-setup-windows-x64.exe`（安裝程式，建議）或 `archiver-windows-x64.zip`（免安裝）。
- **Linux**：`archiver-linux-x64.tar.gz`。需要系統已安裝 GTK3 執行環境（檔案對話框使用）。
- **macOS**：`archiver-macos-universal-app.zip`（內含 `Archiver.app`，Apple Silicon 與 Intel 通用）或 `archiver-macos-universal.tar.gz`（只有執行檔）。App 採 ad-hoc 簽章、未經 Apple 公證，第一次開啟請在 Finder 中對 App 按右鍵 → 打開。

Windows 安裝程式的選項：

- 將壓縮檔類型加入「開啟檔案」清單（預設勾選）。
- 加入檔案總管右鍵選單（預設勾選，見下一節）。
- 將 `rarc` 加入 PATH（預設不勾）。
- 預設安裝到 `Program Files\Archiver`（需系統管理員）；選擇「只為目前使用者安裝」則不需管理員，安裝到 `%LOCALAPPDATA%\Programs\Archiver`。
- 靜默安裝（目前使用者）：

  ```
  archiver-setup-windows-x64.exe /VERYSILENT /SUPPRESSMSGBOXES /CURRENTUSER /TASKS=assoc,contextmenu
  ```

- 解除安裝會移除右鍵選單、檔案關聯與 PATH 設定。

### 開啟、解壓縮、建立

1. **開啟**：雙擊已關聯的壓縮檔；或在 Archiver 中按 Ctrl+O（開啟壓縮檔…）；也可以把壓縮檔拖進視窗。
2. **解壓縮**：工具列「解壓縮到」選擇目的地。只要部分項目時，先在清單中選取（Ctrl／Shift 可多選），再使用「指令 → 解壓縮選取項目…」。
3. **建立**：工具列「加入」（沒有開啟壓縮檔時會建立新壓縮檔），或把檔案、資料夾拖進視窗，會開啟建立對話框，可選擇格式、壓縮等級與密碼（ZIP、7z）。

## 功能

### 瀏覽與搜尋

- 檔案清單顯示名稱、大小、封裝後大小、類型、修改日期與 CRC32；點欄位標題排序；雙擊資料夾進入，`..` 回上一層。
- 沒有開啟壓縮檔時即為檔案管理員模式，可瀏覽本機資料夾，雙擊壓縮檔即開啟。
- 「尋找檔案…」（Ctrl+F）依名稱過濾目前的壓縮檔或資料夾。
- 「我的最愛」可收藏目前位置（資料夾或壓縮檔）。清單儲存在使用者設定資料夾（Windows 為 `%APPDATA%\Archiver\favorites.txt`；設定 `ARCHIVER_CONFIG_DIR` 可改變位置）。

### 解壓縮

- 全部解壓縮，或只解壓縮選取的檔案與資料夾（保留壓縮檔內的目錄結構；選取資料夾時連同其內容一起解出）。
- 需要密碼的壓縮檔會要求輸入密碼。
- 批次解壓縮：「指令 → 分別解壓縮到各自資料夾」，或 CLI 的 `rarc xe`。單一項目失敗不會中斷整批，完成後顯示成功與失敗數。
- 解壓縮時還原 ZIP、7z 內的修改時間；ZIP 解壓縮使用多執行緒。

### 建立與修改

- 建立 ZIP、7z、TAR、TAR.GZ／TAR.BZ2／TAR.XZ／TAR.ZST，以及單一檔案的 GZ／BZ2／XZ／ZST。可設定壓縮等級。
- 分批壓縮：每個選取的檔案或資料夾各自成為一個壓縮檔（建立對話框勾選「每個檔案／資料夾分別建立壓縮檔」，或 「指令 → 分批壓縮」）。
- 加入檔案到既有壓縮檔：ZIP 直接附加；7z 與 TAR.* 會重寫整個壓縮檔，檔案大時較慢。
- 刪除壓縮檔內的項目（ZIP、7z、TAR.*）。
- 壓縮檔註解（僅 ZIP）：工具列「註解」。
- 測試壓縮檔（完整性檢查）。
- 防毒掃描（僅 Windows）：使用 Microsoft Defender 的 `MpCmdRun.exe` 掃描目前開啟的壓縮檔，只回報、不清除。找不到 Defender 時選單不會出現此項目。
- 密碼：ZIP 與 7z 建立時使用 AES-256（7z 同時加密檔名標頭）；ZIP、7z、RAR 開啟時可輸入密碼。

### 預覽與剪貼簿

- 預覽窗格（工具列「預覽」，或「選項 → 預覽窗格」）：不必解壓縮就能看內容。
  - 文字（UTF-8，或含 BOM 的 UTF-8／UTF-16）最多顯示前 256 KiB。
  - PNG、JPEG、SVG 圖片（32 MiB 以內）。
  - 其他內容以十六進位顯示。
  - 資料夾顯示檔案數與總大小；加密項目會先要求密碼。
- 「複製」（工具列或 Ctrl+C）把選取的項目以檔案形式放上系統剪貼簿，可直接在檔案總管、Finder 或檔案管理員中貼上。需要時會先解壓到暫存資料夾（Windows：`%TEMP%\archiver-clipboard`；Linux／macOS：`~/.cache/archiver-clipboard`），啟動時會清除超過 24 小時的暫存內容。
- 文字內容可用「複製文字」複製。

### 鍵盤

| 按鍵 | 功能 |
|---|---|
| Enter | 進入所選資料夾或開啟所選項目 |
| Backspace | 上一層 |
| Delete | 刪除所選項目（可寫入的壓縮檔） |
| Ctrl+A | 全選 |
| ↑ ／ ↓ | 移動選取 |
| Ctrl+O | 開啟壓縮檔 |
| Ctrl+C | 複製到剪貼簿 |
| Ctrl+F | 尋找檔案 |
| F3 | 以預設程式檢視（會先解壓到暫存資料夾） |
| F5 | 重新整理 |
| Alt + 選單字母 | 開啟選單（F、C、S、O、N、H） |

## Windows 右鍵選單

安裝程式預設會加入右鍵選單。也可以在 Archiver 的「選項 → 右鍵選單整合…」勾選，或在終端機執行：

```
archiver --register-shell     # 安裝（目前使用者，不需系統管理員）
archiver --unregister-shell   # 移除
```

在檔案總管中，項目會放在名為 **Archiver** 的子選單內。「壓縮檔」指 Archiver 認得的所有壓縮副檔名（見下方支援格式）；「其他檔案」與「資料夾」只會看到 04、07、08。

| 編號 | 選單文字 | 執行的參數 | 適用對象 | 多選行為 |
|---|---|---|---|---|
| 01 | 以 Archiver 開啟 | `--open` | 壓縮檔 | 只在單選時顯示 |
| 02 | 解壓縮到這裡 | `--extract-here` | 壓縮檔 | 合併成一次作業 |
| 03 | 解壓縮到「壓縮檔名稱\」 | `--extract-to-folder` | 壓縮檔 | 合併成一次作業 |
| 04 | 加入壓縮檔… | `--compress` | 壓縮檔、其他檔案、資料夾 | 合併成一個建立對話框 |
| 05 | 解壓縮至… | `--extract-to` | 壓縮檔 | 合併，只選一次目的地 |
| 06 | 測試壓縮檔 | `--test` | 壓縮檔 | 合併成一次作業 |
| 07 | 壓縮成同名 .zip | `--compress-to zip` | 壓縮檔、其他檔案、資料夾 | 合併成一次作業 |
| 08 | 壓縮成同名 .7z | `--compress-to 7z` | 壓縮檔、其他檔案、資料夾 | 合併成一次作業 |

說明：

- **多選合併**：Explorer 會對每個選取的項目各啟動一次程式。Archiver 以 `--collect` 收集這些路徑，只開一個視窗、執行一次作業。收集的時間窗很短（約 250 毫秒無新項目即結束，最長等待 3 秒）。
- **Windows 11**：新版右鍵選單不會顯示 Archiver 的完整項目。請點「顯示其他選項」（或按 Shift+F10／Shift+右鍵）。目前沒有提供新式選單所需的簽章套件。
- **寫入位置**：只寫入 `HKEY_CURRENT_USER\Software\Classes\` 之下，不需系統管理員。包括：
  - `SystemFileAssociations\.<副檔名>\shell\Archiver`：每個壓縮副檔名一組。
  - `*\shell\Archiver`：其他檔案（以 AppliesTo 排除壓縮檔）。
  - `Directory\shell\Archiver`：資料夾。
  - 每個項目有 `MUIVerb`、`Icon`、`MultiSelectModel`；指令寫在 `command` 子鍵中，格式為 `"<archiver.exe 路徑>" --collect <參數> "%1"`（01 不含 `--collect`）。
- 註冊前會先刪除上述根鍵再重建，因此舊版留下的項目會消失；註冊時也會移除 v0.2.0「Rust Archiver」的舊項目。
- 註冊時寫入的是目前 exe 的完整路徑，搬移程式後請重新執行 `--register-shell`。

Linux 與 macOS 使用同一組動作，安裝的內容如下：

| 系統 | 安裝內容 |
|---|---|
| Linux | `~/.local/share/applications/archiver.desktop`（MIME 關聯，「以其他程式開啟」）、Nautilus 腳本（`~/.local/share/nautilus/scripts/Archiver/`）、Dolphin 服務選單（KF6 與 KF5 路徑）、Nemo 動作 |
| macOS | `~/Library/Services/Archiver - *.workflow`（Finder 快速動作）。若沒有出現，請到系統設定中確認已啟用 |

## 支援格式

| 格式（副檔名） | 建立 | 解壓縮與測試 | 加入／刪除項目 | 密碼（建立／開啟） | 壓縮等級 |
|---|:---:|:---:|:---:|:---:|:---:|
| ZIP（`.zip`、`.jar`） | ✅ | ✅ | ✅（附加） | ✅ AES-256／✅ | 0–9 |
| 7z（`.7z`） | ✅ | ✅ | ✅（重寫） | ✅ AES-256、含標頭加密／✅ | 0–9 |
| TAR（`.tar`） | ✅ | ✅ | ✅（重寫） | — | 無 |
| TAR.GZ（`.tar.gz`、`.tgz`） | ✅ | ✅ | ✅（重寫） | — | 0–9 |
| TAR.BZ2（`.tar.bz2`、`.tbz2`、`.tbz`） | ✅ | ✅ | ✅（重寫） | — | 0–9 |
| TAR.XZ（`.tar.xz`、`.txz`） | ✅ | ✅ | ✅（重寫） | — | 0–9 |
| TAR.ZST（`.tar.zst`、`.tzst`） | ✅ | ✅ | ✅（重寫） | — | 1–22 |
| GZ／BZ2／XZ／ZST（單一檔案） | ✅ | ✅ | ✗ | — | GZ／BZ2／XZ 為 0–9；ZST 為 1–22 |
| RAR（`.rar`） | ✗ | ✅ | ✗ | —／✅ | 無 |

> **為什麼不能建立 RAR？** RAR 壓縮演算法是 RARLAB 的專有格式，UnRAR 的授權只允許解壓縮，不允許重建 RAR 壓縮器，因此本程式只能讀取 RAR。

## 命令列

### `archiver`（圖形介面程式）

```
Usage: archiver [--collect] [OPTION] [FILES...]
  <archive>                       open archive in the GUI
  --open <archive>                open archive in the GUI
  --extract-here <archives...>    extract next to each archive
  --extract-to-folder <archives...>  extract into <dir>/<archive name>/
  --extract-to <archives...>      choose a destination folder for extraction
  --compress <files/folders...>   open the create-archive dialog prefilled
  --compress-to <fmt> <files...>  compress next to the inputs without a dialog
                                  (fmt: zip, 7z, tar, tar.gz, tar.bz2, tar.xz, tar.zst, gz, bz2, xz, zst)
  --test <archives...>            test archive integrity
  --screenshot <out.png> [--size WxH] [archive-or-folder]
                                  render the main window to a PNG and exit
  --collect                       (before an option) Explorer multi-select: merge the paths
                                  passed by every process of one selection into a single run
  --register-shell                install right-click menu integration (current user)
  --unregister-shell              remove right-click menu integration
  -h, --help                      show this help
```

- 只給一個路徑時會開啟它；給多個一般路徑（例如拖到程式圖示上）會建立壓縮檔。
- `--screenshot` 會以 CPU 繪製器把主視窗輸出為 PNG 後結束，預設大小 1100×720，不需要 GPU 或顯示器的視窗。

### `rarc`（命令列工具）

```
rarc [--json] <command>

  l  <archive> [-p 密碼]                              列出內容
  t  <archive> [-p 密碼]                              測試壓縮檔
  x  <archive> [-o 輸出資料夾] [-p 密碼] [項目...]     解壓縮（可只解指定項目）
  c  <輸出> <輸入...> [-l 等級] [-p 密碼]              建立（格式依輸出副檔名）
  a  <壓縮檔> <輸入...> [--prefix 資料夾] [-l 等級] [-p 密碼]   加入檔案（同名項目會被取代）
  comment show <壓縮檔>                               顯示註解（ZIP）
  comment set <壓縮檔> -m "註解"                       設定註解（空字串會刪除）
  ce <輸入...> -f <格式> [-o 輸出資料夾] [-l 等級] [-p 密碼]   分批壓縮（每個輸入各成一個壓縮檔）
  xe <壓縮檔...> [-o 輸出資料夾] [-p 密碼]             分批解壓縮（各自解到 <壓縮檔名>/ 資料夾）
```

範例：

```
rarc l archive.7z -p 密碼
rarc x archive.rar -o 輸出資料夾
rarc c out.tar.zst 資料夾 檔案.txt -l 19
rarc a backup.zip 新檔案.txt --prefix docs
rarc ce a.txt 照片 文件 -f zip -o 輸出資料夾
rarc xe *.zip *.7z -o 輸出資料夾
```

**給程式使用**：加上 `--json` 時，每個命令在 stdout 輸出一份 JSON 文件（一行）。

- `l`：`{"ok":true,"archive":…,"count":N,"entries":[{"name","size","compressed","modified","is_dir","crc32","encrypted"}]}`
- `t`：`{"ok":true,"archive":…,"tested":N}`；`x`：`{"ok":true,"archive","out"}`；`c`：`{"ok":true,"output"}`；`a`：`{"ok":true,"archive","prefix","added"}`
- `comment show`：`{"ok":true,"archive","comment"}`（沒有註解時為 null）；`comment set`：`{"ok":true,"archive"}`
- `ce`／`xe`：`{"ok","total","failed","results":[{"input","output","ok","error","kind"}]}`
- 失敗：`{"ok":false,"error":"…","kind":"…"}`，其中 kind 為 `password`、`io`、`unsupported`、`cancelled`、`not_found`、`format` 之一。

**結束代碼**

| 代碼 | 意義 |
|:---:|---|
| 0 | 成功 |
| 1 | 失敗 |
| 2 | 用法錯誤（參數不正確；由 clap 輸出到 stderr） |
| 3 | 需要密碼，或密碼錯誤 |
| 4 | 批次部分失敗（`ce`、`xe`：有的成功、有的失敗） |

`ce`、`xe` 全部失敗時，若失敗原因都是密碼問題則回傳 3，否則回傳 1。

## 從原始碼建置

### 需求

- Rust 1.85 以上（專案使用 edition 2024）。CI 使用 stable。
- **Windows**：需要 MSVC 工具鏈與 Visual Studio Build Tools。若預設工具鏈是 GNU（`-gnu`），連結可能失敗，建議：

  ```
  rustup toolchain install stable-x86_64-pc-windows-msvc
  cargo +stable-x86_64-pc-windows-msvc build --release
  ```

- **Linux**（與 CI 相同的套件）：

  ```
  sudo apt-get install -y --no-install-recommends pkg-config libgtk-3-dev \
    libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libwayland-dev libfontconfig1-dev
  ```

- **macOS**：Xcode Command Line Tools。通用版 `.app` 使用 `scripts/macos-bundle.sh`（同時建置 arm64 與 x86_64）。
- **C/C++ 編譯器**：RAR 讀取使用 `unrar` crate，內含 UnRAR 的 C++ 原始碼。

### 指令

```
cargo build --release --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

- 建置結果：`target/release/archiver`（Windows 為 `archiver.exe`）與 `rarc`。
- 自行打包 Windows 安裝程式：先 `cargo build --release`，再執行 `pwsh scripts/build-installer.ps1`。需要 Inno Setup 6（找不到時會嘗試以 choco 安裝），輸出到 `dist\archiver-setup-windows-x64.exe`。

### AcmeUI-Native 相依

GUI 相依於 AcmeUI-Native 的 git 版本，已釘定在 `c2e0fd9765d0f25119a240ad1adb378c045df63f`（見 `gui/Cargo.toml`）。第一次建置需要能連到 GitHub。

### 渲染後端

由環境變數 `ACME_RENDER_BACKEND` 選擇，可設為 `auto`、`gpu` 或 `cpu`（預設 `auto`）：

- `auto`：先使用 GPU（wgpu）；無法使用時退回 CPU 軟體繪製。
- `gpu`：優先使用 GPU，同樣會退回 CPU。
- `cpu`：強制使用 CPU 繪製。

沒有顯示卡或在遠端桌面中遇到問題時，可以設定 `ACME_RENDER_BACKEND=cpu`（PowerShell：`$env:ACME_RENDER_BACKEND = 'cpu'`）。`--screenshot` 一律使用 CPU 繪製。

### 測試

`cargo test --workspace` 包含：

- `core` 的壓縮與解壓縮測試。
- GUI 狀態機測試（`gui/src/tests.rs`），不需要開啟視窗。
- 右鍵選單登錄測試（Windows 版在暫存的登錄路徑上執行）與多選合併測試。
- `rarc` 的整合測試（`gui/tests/rarc_cli.rs`）。

剪貼簿測試預設略過（需要顯示環境與系統剪貼簿）。

## 專案結構

```
core/                     archiver-core 函式庫（格式偵測、列出、解壓、測試、建立、加入／刪除、註解、批次）
  src/lib.rs              Format、Error、Entry、Progress 與公開 API
  src/archive_ops.rs      列出、解壓縮、測試、刪除
  src/create.rs           建立壓縮檔
  src/edit.rs             註解與加入檔案
  src/batch.rs            批次壓縮與解壓縮
  src/preview.rs          預覽用的單一項目讀取
gui/                      archiver（圖形介面）與 rarc（命令列）
  src/main.rs             進入點：參數分派、--screenshot、事件迴圈（App 實作 AcmeUI Application）
  src/args.rs             命令列參數解析與說明文字
  src/app/                狀態機
    model.rs              Model：所有狀態
    msg.rs                Msg、Cmd、按鍵與對話框訊息
    update.rs             update()：狀態轉換
    effects.rs            Effects 介面與 RealEffects（檔案對話框、執行緒、喚醒）
    menus.rs              選單與右鍵選單的內容（資料）
    worker.rs             背景工作輔助
  src/ui/                 即時模式繪製：版面、對話框、元件、文字、圖示
  src/shell.rs            右鍵選單：Windows 登錄、Linux 與 macOS 檔案產生
  src/collect.rs          Explorer 多選合併
  src/favorites.rs        我的最愛
  src/scan.rs             Microsoft Defender 掃描（Windows）
  src/view.rs             清單資料、暫存與剪貼簿資料夾
  src/preview.rs          預覽內容分類（文字、圖片、十六進位）
  src/clipboard.rs        系統剪貼簿（檔案與文字）
  src/os.rs               以預設程式開啟檔案、系統深淺色偵測
  src/tests.rs            GUI 狀態機測試
  src/bin/rarc.rs         rarc 命令列工具
  tests/rarc_cli.rs       rarc 整合測試
  assets/                 SVG 圖示與 Windows 圖示
  build.rs                Windows 資源（圖示）
installer/                Inno Setup 腳本
scripts/                  安裝程式與 macOS 打包腳本、安裝程式煙霧測試
bench/                    效能測試腳本與說明
docs/                     截圖、圖示與變更紀錄（CHANGELOG.md）
.github/workflows/        CI 與 Release
```

## 已知限制

- **RAR 只能讀取**（授權限制）。不支援建立分卷壓縮檔；7z 分卷與自解壓（SFX）檔也不支援。
- **單一檔案格式**（GZ、BZ2、XZ、ZST）不能加入或刪除項目；需要多個檔案時請使用 TAR.* 或 ZIP、7z。
- **Windows 右鍵選單**：
  - Explorer 會對每個選取項目各啟動一次程式，合併依賴短暫的時間窗；系統極慢時仍可能分成多個視窗或作業。
  - Windows 11 的新式選單不顯示 Archiver 項目，需要「顯示其他選項」。
  - 「解壓縮到「壓縮檔名稱\」」是固定文字，無法依實際選取的壓縮檔動態改名。
- **macOS「以 Archiver 開啟」／雙擊**：App 會啟動，但不會自動載入檔案，因為 winit 0.30（本專案鎖定版本）沒有提供開啟檔案的事件。請改用 Finder 快速動作，或把檔案拖進視窗。
- **Linux 與 macOS 的檔案管理員整合**（Nautilus、Dolphin、Nemo、Finder 快速動作）只以產生檔案內容與格式的測試驗證，尚未在實際桌面上逐一點擊測試。
- **防毒掃描**只支援 Windows Defender，只回報、不清除。
- **註解**只支援 ZIP。
- **預覽**：非 UTF 編碼的文字（例如 Big5、GBK）以十六進位顯示；圖片限 PNG、JPEG、SVG。
- **輸入法**：注音輸入法在 AcmeUI 文字欄位中尚未實機驗證。
- **介面語言**目前只有繁體中文。
- **macOS** 的 App 採 ad-hoc 簽章、未經 Apple 公證。

## 效能

v0.4 的測試（Linux x86_64，8 核心，約 490 MB 混合資料）：7z 建立 292 秒 → 46 秒，tar.xz 285 秒 → 41 秒，zip 10.5 秒 → 2.4 秒，tar.gz 9.4 秒 → 1.6 秒，tar.zst 2.0 秒 → 0.55 秒。多執行緒的 7z、xz 壓縮記憶體用量較高（約 1.2–1.4 GB）；可設定環境變數 `ARCHIVER_THREADS` 限制執行緒數。完整數據與方法見 [bench/README.md](bench/README.md)。

## 授權

MIT License，見 [LICENSE](LICENSE)。

---

## English summary

**Archiver** is a cross-platform (Windows, Linux, macOS) archive manager with a WinRAR-style window. The workspace has three parts: `archiver-core` (library), `archiver` (GUI, built on AcmeUI-Native), and `rarc` (command-line tool). Release notes are in [docs/CHANGELOG.md](docs/CHANGELOG.md).

- **Formats**: create ZIP, 7z, TAR (with gz, bz2, xz, zst) and single-file gz, bz2, xz, zst archives. ZIP and 7z support AES-256 passwords. RAR is read-only because the UnRAR license forbids creating RAR archives.
- **Features**: list, test, extract (all or selected entries), add and delete entries in ZIP, 7z and TAR archives, ZIP comments, live preview, copy to the system clipboard, batch compress and extract, favorites, drag and drop, and Microsoft Defender scanning on Windows.
- **Shell integration** (current user, no admin): Windows Explorer submenu under HKCU; Nautilus, Dolphin and Nemo on Linux; Finder Quick Actions on macOS. Install with `archiver --register-shell`. Multi-selection runs as a single operation.
- **CLI**: `rarc` supports `--json` for every command and fixed exit codes: 0 ok, 1 failed, 2 usage error, 3 password required or wrong, 4 partial batch failure.
- **Build**: Rust 1.85 or newer. On Windows use the MSVC toolchain. `cargo build --release --locked`. The renderer uses wgpu and falls back to the CPU; set `ACME_RENDER_BACKEND=auto|gpu|cpu` to choose.
- **Limitations**: RAR is extract-only; no multi-volume or SFX creation; on macOS, "Open With" starts the app without loading the file (winit 0.30 has no open-file event); on Windows 11 the context menu needs "Show more options"; the GUI text is Traditional Chinese only. See the 已知限制 section above.

