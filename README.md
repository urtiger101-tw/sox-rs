# soundx — Rust 原生音訊處理工具

> **SoX 的精神 · Rust 的安全 · 現代化的設計**

`soundx` 是 SoX 風格的純 Rust 音訊處理器，目標是逐步實現命令列、格式、效果與裝置相容。
目前仍是功能子集，尚未完整相容 SoX；請以本文件列出的支援範圍為準。

---

## 快速開始

```bash
# 從原始碼建置
git clone https://github.com/stevenke1981/sox-rs.git
cd sox-rs
cargo build --release
./target/release/soundx --help

# 安裝至 Cargo 的 bin 目錄
cargo install --path .
```

---

## 使用範例

```bash
# 檢視音訊資訊
soundx info input.wav
soundx info --json input.wav

# 轉換 + 效果處理
soundx convert input.wav output.wav --gain-db -3 --trim 0 10 --normalize

# headerless RAW requires its sample rate and channel count
soundx convert --input-raw-rate 48000 --input-raw-channels 2 --input-raw-encoding pcm-s24be input.raw output.wav
soundx convert --output-raw-encoding float32be input.wav output.raw

# 合成音訊
soundx synth tone.wav --duration 2 --freq 440 --waveform sine

# 串接與混音
soundx concat -o album.wav intro.wav body.wav outro.wav
soundx mix -o bed.wav voice.wav music.wav --normalize

# 批次處理
soundx batch "samples/*.wav" --out-dir out --normalize --rate 48000

# JSON 處理計畫
soundx run-plan plan.json

# 列舉裝置、多檔/循環播放與錄音
soundx devices
soundx play intro.wav chapter1.wav chapter2.wav
soundx play playlist-a.wav playlist-b.wav --loop
soundx record take.wav --duration 10
soundx record live.wav --continuous

# 純 Rust 壓縮格式
soundx convert input.wav compressed.wav --wav-adpcm ima
soundx convert input.wav speech.gsm
soundx convert input.wav speech.amr
soundx convert input.wav speech-wide.awb
soundx convert input.wav lossless.wv

# SoX 風格命令列（相容模式）
soundx input.wav output.wav gain -6 trim 0 0.01 pad 0.01 0.01 speed 1.5 rate 22050

# 串流處理（適合大檔案）
soundx stream input.wav output.wav --gain-db=-3 --fade-in 0.01 --limiter 0.7
```

### 支援的效果

| 效果 | 說明 |
|------|------|
| `gain` / `vol` | 音量增益（dB） |
| `normalize` | 正規化至目標音量 |
| `trim` | 裁剪音訊區段 |
| `fade` | 淡入淡出 |
| `reverse` | 反轉音訊 |
| `speed` | 變速（線性插值） |
| `stretch` / `tempo` | WSOLA 時間伸縮，保留近似音高 |
| `dither` | TPDF 抖動並量化到指定位元深度 |
| `compand` | attack/decay 包絡與 dB transfer curve 壓縮 |
| `reverb` | Freeverb 風格混響，可輸出 wet-only |
| `pad` | 填補靜音 |
| `silence` | 移除前後靜音 |
| `lowpass` | 低通濾波器 |
| `highpass` | 高通濾波器 |
| `bass` / `treble` | 二階低架／高架音調濾波器 |
| `allpass` / `bandpass` / `bandreject` | 二階全通、帶通與帶阻濾波器 |
| `equalizer` | 二階參數式峰值等化器 |
| `echo` | 一組或多組毫秒延遲/衰減 tap |
| `tremolo` | 正弦振幅調變 |
| `delay` | 依秒數延後全部聲道或分別延後聲道 |
| `dcshift` | 加入或移除直流偏移 |
| `downsample` / `upsample` | 抽取樣本或插入零樣本並調整取樣率 |
| `repeat` | 重複完整輸入音訊指定次數 |
| `swap` | 交換前兩個聲道 |
| `limiter` | 限制器（hard-clamp） |
| `rate` | 重取樣 |
| `channels` | 聲道轉換 |

### 支援的格式

| 方向 | 格式 |
|------|------|
| 讀取 | WAV（PCM/float、IMA/MS ADPCM）、GSM 06.10、AMR-NB/WB、WavPack v5、AIFF、AU/SND、RAW、FLAC、MP3、Ogg/Vorbis、Opus、AAC、ALAC、CAF、MKV/WebM、MP4/M4A |
| 寫入 | WAV（PCM/float、IMA/MS ADPCM）、GSM 06.10、AMR-NB/WB、WavPack v5、FLAC、MP3、Ogg/Vorbis、AAC/ADTS、AIFF、AU/SND、RAW |
| 串流 | WAV → WAV（gain, fade, limiter） |
| 裝置 | 系統裝置列舉、多檔播放、循環播放、定時/連續 WAV 錄音；可選裝置名稱、取樣率與聲道數 |

格式與效果是 SoX 風格子集，細節、參數和樣本結果不保證與 SoX 完全一致；請參閱 [SoX 相容性矩陣](docs/SOX_COMPATIBILITY.md)。WavPack 僅支援 v5 lossless mono/stereo；AMR 使用 `.amr`/`.awb` 單聲道 storage frames。多檔播放會先在記憶體中串接；連續錄音以 bounded queue 寫 16-bit PCM WAV。錄音需要作業系統授權及可用輸入裝置；播放/錄音使用 CPAL 對接作業系統音訊後端。
RAW 輸入沒有檔頭，需在 `convert` 提供 `--input-raw-rate` 與 `--input-raw-channels`；樣本格式預設為 `pcm-s16le`。RAW 輸出預設 `pcm-s16le`，可用 `--output-raw-encoding` 選擇格式。AU/SND 可用 `--au-encoding pcm8|pcm16|pcm24|pcm32|float32|float64|mu-law|a-law` 選擇編碼。
WAV 可用 `convert --bits 64 --float` 輸出 64-bit float；目前處理管線使用 32-bit float 樣本。AIFF 輸出預設為 16-bit PCM，可用 `convert --aiff-bits 8|16|24|32` 選擇位元深度。

---

## 架構

```
soundx/            # ── crate root
├── src/
│   ├── main.rs    # 入口、命令分發
│   ├── cli.rs     # CLI 參數解析
│   ├── audio.rs   # AudioBuffer、WAV/AU/RAW I/O、Symphonia 解碼
│   ├── encode.rs  # WAV、FLAC、MP3、Vorbis、AAC、AIFF、AU、RAW 編碼
│   ├── device.rs  # 裝置列舉、播放、定時錄音
│   ├── effects.rs # Effect 枚舉、EffectChain、DSP 實作
│   ├── parse.rs   # 效果 token 解析器
│   ├── io.rs      # 檔案 I/O 公用函式
│   ├── mix.rs     # 串接與混音
│   ├── streaming.rs # 增量 WAV 管線
│   ├── synth.rs   # 波形合成
│   ├── stats.rs   # 音訊統計報告
│   └── util.rs    # 共用工具函式
├── tests/
│   ├── cli.rs     # CLI 整合測試
│   ├── effects.rs # 效果單元測試
│   └── common/    # 測試輔助工具
└── docs/
    ├── ARCHITECTURE.md
    ├── KNOWLEDGE_MAP.md
    └── ROADMAP.md
```

四條處理路徑：
1. **AudioBuffer 路徑**：將輸入解碼為 `f32` 交錯樣本，套用效果鏈，依副檔名編碼
2. **Streaming 路徑**：增量處理 WAV 樣本，適合大檔案
3. **Synth 路徑**：生成內建波形，重複使用效果鏈與格式編碼器
4. **Device 路徑**：CPAL 提供輸入/輸出裝置與音訊串流

---

## 安裝包

發行流程會產生以下平台的執行檔封裝；裝置功能使用各作業系統的音訊後端：

| 平台 | 格式 |
|------|------|
| Windows x86_64 | `soundx-x.y.z-x86_64-pc-windows-msvc.zip`、`soundx-x.y.z-setup.exe` |
| Linux x86_64 | `soundx-x.y.z-x86_64-unknown-linux-gnu.tar.gz`（`soundx`） |
| macOS x86_64 | `soundx-x.y.z-x86_64-apple-darwin.tar.gz`（`soundx`） |

### 從原始碼建置

```bash
cargo build --release
# 執行檔位於 target/release/soundx (或 soundx.exe)
```

Windows 安裝程式會將安裝目錄加入 PATH；預設安裝至目前使用者，安裝精靈亦可選擇系統層級安裝（需管理員權限）。請在安裝後開啟新的終端機，執行 `soundx --version` 驗證。

以 `pwsh -File scripts/package.ps1` 建立 ZIP 與 Inno Setup 安裝程式。Inno Setup 6 或 7 須已安裝。

Linux 建置需安裝 `pkg-config` 與 ALSA 開發套件（Debian/Ubuntu：`sudo apt-get install pkg-config libasound2-dev`）。封裝包含相容性說明、中英文使用文件及第三方授權聲明。

`play --loop` 持續播放到 Ctrl+C；播放清單先載入記憶體，第一個音軌直接移轉緩衝區以減少複製。連續錄音遇到 RIFF WAV 的 4 GiB 上限會停止並回報錯誤，保存完整音框與可讀取的 WAV 標頭；目前不輸出 RF64。

目前提供 SoX 風格的部分命令與效果，尚未達到完整 SoX 格式、效果及參數相容；請以 `soundx --help` 與 `soundx formats` 檢查已實作範圍。

---

## 授權

**LGPL-2.1-or-later** 或 **MIT**（任選其一）。

---

## 相關連結

- [English Documentation](pages/en/README.md)
- [繁體中文說明](pages/zh-TW/README.md)
- [架構文件](docs/ARCHITECTURE.md)
- [知識圖譜](docs/KNOWLEDGE_MAP.md)
- [開發藍圖](docs/ROADMAP.md)
- [SoX 相容性現況與差距](docs/SOX_COMPATIBILITY.md)
