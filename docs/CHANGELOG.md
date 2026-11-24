# 變更紀錄

## v0.8.0

主題：WinRAR 功能對齊。

### 新功能

- 解壓縮覆寫模式：「覆寫現有檔案」「略過現有檔案」「自動重新命名」（新檔名為 `名稱 (2).副檔名`，依序遞增）。在「選項」選單設定，套用到 GUI 與右鍵選單的解壓縮；CLI 以 `rarc x --overwrite` 與 `rarc xe --overwrite` 指定。
- 重新命名（F2）：支援 ZIP、7z、TAR.*。ZIP 與 TAR.* 直接複製項目、不需重新壓縮；7z 會重寫壓縮檔（加密的 7z 需要密碼）。資料夾檢視中可重新命名檔案與資料夾。
- 選取群組、取消選取群組、反向選取（Gray +、Gray −、Gray *，也在「檔案」選單）。遮罩支援 `*` 與 `?`，多個遮罩以 `;` 分隔；`*.*` 等同全部。
- 轉換壓縮檔（Alt+Q，「工具」選單）：把壓縮檔轉換為另一種格式，來源檔保留。
- 產生報告（Alt+G，「工具」選單）：輸出 TXT、CSV 或 HTML（依副檔名決定）。
- 另存壓縮檔複本（「檔案」選單）。
- 最近開啟的壓縮檔（「檔案」選單，最多 8 個）。
- 設定保存在 `settings.json`，與 `favorites.txt` 放在同一個設定資料夾（Windows 為 `%APPDATA%\Archiver\`；可用 `ARCHIVER_CONFIG_DIR` 改變）。保存的項目：預設格式、預覽窗格、解壓縮覆寫模式、壓縮後測試、最近開啟清單。
- 建立對話框新增「壓縮後測試壓縮檔」與「壓縮後刪除原始檔案」。刪除前會先確認，且為永久刪除（不進資源回收筒）。
- WinRAR 快捷鍵：
  - Alt+A 加入檔案到壓縮檔
  - Alt+E 解壓縮至…
  - Alt+W 解壓縮到這裡
  - Alt+T 測試壓縮檔
  - Alt+V 檢視檔案
  - Alt+M 註解
  - Alt+I 或 Alt+Enter 顯示資訊
  - Alt+Z 精靈
  - Alt+Q 轉換壓縮檔
  - Alt+G 產生報告
  - F2 重新命名
  - Gray +、Gray −、Gray * 選取群組、取消選取群組、反向選取
  - Ctrl+P 設定預設密碼
  - Ctrl+PageUp 上一層
- 選單依 WinRAR 重新排列：「防毒掃描」移到「工具」選單。
- 命令列 `rarc`：
  - 新增 `rn <壓縮檔> <原名稱> <新名稱>`（壓縮檔內重新命名），成功時輸出 `{"ok":true,"archive","from","to"}`。
  - 新增 `cv <輸入> [輸出] [-f 格式]`（轉換壓縮檔），成功時輸出 `{"ok":true,"input","output"}`。
  - `x` 與 `xe` 新增 `--overwrite replace|skip|rename`。
  - `x` 的成功輸出新增 `skipped` 與 `renamed` 欄位。

### 修正

- ZIP 刪除項目後會保留壓縮檔註解。
- 含 ZipCrypto（傳統 ZIP 加密，WinRAR／7-Zip／檔案總管常用）項目的 ZIP：先前加入或刪除項目會把這些項目寫成「未加密」而損毀。現在重新命名與刪除會拒絕並保留原檔；加入檔案改為直接附加，原有加密項目逐位元組保留。需要修改時請先轉換成 AES 加密的 ZIP 或 7z。
- TAR 加入／重新命名：重建標頭，修正 ustar `prefix` 欄位造成的錯誤路徑，並保留 pax 擴充（次秒級時間、xattr）。
- 產生 CSV 報告時，以 `= + - @` 開頭的名稱會加上 `'`，避免在 Excel 中被當成公式。
- 暫存檔改用唯一名稱，不會覆寫使用者同名檔案；設定檔改在背景寫入，不會卡住畫面。

### 其他

- 不再支援 Windows 7／8.1。Rust 1.78 起的 Windows 標準工具鏈與圖形堆疊（wgpu／winit）需要 Windows 10 以上。

## v0.7.1

### 修正

- 「解壓縮至…」在沒有開啟或選取壓縮檔時，選好壓縮檔後只會開啟它而不解壓縮。現在會接著詢問目的資料夾並直接解壓縮；需要密碼時改為開啟壓縮檔以便輸入密碼。

### CI

- 將使用 Node.js 20 的 GitHub Actions 升級：`actions/checkout` v7、`actions/upload-artifact` v7、`softprops/action-gh-release` v3。

## v0.7.0

### 重大變更

- GUI 改用 [AcmeUI-Native](https://github.com/stevenke1981/AcmeUI-Native) 繪製，取代 Slint。釘定 rev `c2e0fd9`。GPU（wgpu）優先，無法使用時自動改用 CPU；可用 `ACME_RENDER_BACKEND=auto|gpu|cpu` 指定。
- GUI 重新整理為 Model / Msg / update / Effects 架構，狀態邏輯可在無視窗環境下以測試驗證。
- 移除 Slint 相關程式、`gui/ui/app.slint` 與 `scripts/win-screenshot.ps1`。截圖改由 `archiver --screenshot` 產生（CPU 繪製，不需 GPU）。

### 新功能

- Windows 右鍵選單：新增「測試壓縮檔」「壓縮成同名 .zip」「壓縮成同名 .7z」，每個項目都有圖示，`CommandFlags` 分隔線。既有項目的編號（01–05）保持不變。
- Explorer 多選合併：多選後只開一個視窗、執行一次作業（`--collect`）。
- 加入檔案到既有壓縮檔：ZIP 直接附加；7z 與 TAR.* 重寫壓縮檔。
- ZIP 壓縮檔註解的讀取與設定（工具列「註解」）。
- Windows 防毒掃描（Microsoft Defender `MpCmdRun.exe`，只回報不清除）。
- 我的最愛（「我的最愛」選單）。
- 命令列 `archiver`：`--test`、`--compress-to <格式>`、`--collect`、`--screenshot <png> [--size WxH] [路徑]`。
- 命令列 `rarc`：所有命令支援 `--json`；新增 `t`、`a`、`comment show|set`；結束代碼定為 0／1／2／3（需要或輸入錯誤密碼）／4（批次部分失敗）。

### 修正與品質

- CI 以 `cargo clippy --workspace --all-targets -- -D warnings` 檢查；修正 core 的 clippy 警告。
- CI 新增 rarc JSON 煙霧測試、右鍵選單登錄與移除的煙霧測試（含舊版「Rust Archiver」項目清除），截圖步驟改為 `--screenshot`。
- 右鍵選單註冊時會先刪除既有的根鍵，因此已移除的舊項目不會殘留。
- README 修正：macOS「以 Archiver 開啟」不載入檔案的原因是 winit 0.30 沒有開啟檔案事件（先前誤寫為 eframe）。

### 移除

- SFX（自解壓）按鈕（尚未支援）。

## v0.6.1

- 右鍵選單新增「解壓縮至…」，可自選目的地。
- 可只解壓縮選取的檔案或資料夾（「解壓縮選取項目…」）。
- 安裝程式與說明文件更新。

## v0.6.0

- 預覽窗格：文字、PNG／JPEG／SVG 圖片、十六進位傾印，不需先解壓縮。
- 複製到系統剪貼簿（以檔案形式），可直接在檔案總管或 Finder 貼上。
- 分批壓縮（建立對話框的分別建立、`rarc ce`）與分批解壓縮（`rarc xe`）。
- ZIP 解壓縮平行處理；只解壓縮選取項目時不開啟未選取的項目；還原 ZIP、7z 的修改時間。
- 新增鍵盤快捷鍵：方向鍵、Ctrl+C、Ctrl+F、F3、F5。

## v0.5.2

- 修正 Windows 上的渲染器選擇（Slint 時期的修正）。

## v0.5.1

- Windows Inno Setup 安裝程式（繁體中文與英文介面）；程式內嵌應用程式圖示。

## v0.5.0

- GUI 改用 Slint，仿 WinRAR 的主視窗：選單列、大圖示工具列、路徑列、可排序的檔案清單、狀態列。
- 新增測試壓縮檔、刪除、資訊、檢視。

## v0.4.0

- 多執行緒：xz（liblzma）、zstd、7z（固實 LZMA2）、ZIP 平行壓縮、平行 gzip；較大的 I/O 緩衝；fat LTO。
- 新增 `bench/` 效能測試。

## v0.3.0

- 更名為 Archiver；清除舊版 "Rust Archiver" 的右鍵選單項目。

## v0.2.0

- 右鍵選單整合（Windows、Linux、macOS）、命令列動作、macOS 通用 .app。

## v0.1.0

- 初版（名為 Rust Archiver）：core 函式庫、egui 圖形介面、命令列工具、CI 與 Release 工作流程。
