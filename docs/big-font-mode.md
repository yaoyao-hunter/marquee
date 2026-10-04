# Big-Font Mode 设计文档（fusion-pixel-font 超大字幕滚动）

状态：Draft — 待评审
分支：feat/big-font-mode
关联看板：vuv P-1 (marquee)，T-9..T-12（本次新增拆分）

## 1. 目标

在现有 marquee（单行、按 terminal column 滚动）基础上，新增“超大字”模式：
用 [缝合像素字体 / Fusion Pixel Font](https://github.com/TakWolf/fusion-pixel-font)
的点阵字形，把中文/日文文本放大到占多行终端高度，横向平滑滚动：

```
marquee "你好世界" --big
marquee "你好世界" --big --scale 4
echo "建设中" | marquee --big --direction right
```

效果（示意，半块字符渲染，scale=1 时 CJK 字面 12 列 × 6 行）：

```
█▀▀▀▀▀▀█ ...
█    █
█▄▄▄▄▄▄█
```

非目标（V1 不做）：

- 纵向滚动 / 多行字幕排版
- 彩色渐变、阴影、描边
- 比例模式（proportional）字形，只用等宽模式
- 运行时加载用户自选 TTF（只内嵌预处理好的点阵）

## 2. 字体与许可

- 来源：fusion-pixel-font release `2026.09.25`，`12px-monospaced` BDF。
  - `fusion-pixel-font-12px-monospaced-bdf-v2026.09.25.zip`
  - sha256: `d75f5262f108757edb0f47ee8e3d2dfdfbecfd94558faf5ffb0c06dc5866fb3b`
- 语言变体：默认打包 `zh-Hans`，构建资产时可再生成 `ja` / `zh-Hant` 变体。
- 许可：字体为 **OFL-1.1**（上游各源字体均 OFL-1.1 或兼容）。
  义务：仓库内保留 `LICENSE-OFL` 与版权声明（`assets/fusion-pixel/LICENSE`、
  `FONTLOG` 摘要），README 的 Acknowledgements 注明字体来源。
  OFL 允许内嵌/再分发字形数据，不要求我们的代码采用 OFL。
- 等宽模式度量：全角字形 12×12 px，半角（拉丁）6×12 px，基线对齐严格，
  天然适合按列滚动 —— 与 marquee 的 column 模型一致。

## 3. 字形资产管线（build-time）

不把 36MB 的 BDF 塞进仓库，也不在运行时解析 BDF：

1. `tools/gen-bigfont/`（独立 Rust bin，仅开发期运行）读取官方 BDF，
   过滤字符集，输出紧凑二进制 `assets/bigfont-12px-zh-hans.bin`：
   - header：magic、版本、glyph 高(12)、全角宽(12)、半角宽(6)、glyph 数
   - 索引：`char(u32) -> (offset, width_class)`，按 char 排序，运行时二分查找
   - 位图：每 glyph 12 行 × 12 bit，行按 2 字节打包（大端），全角 24B/字，
     半角 12B/字（高 12 行 × 6 bit，仍按行 2 字节对齐存储，简单优先）
2. 字符集（V1）：ASCII 可打印字符 + CJK 统一表意文字基本区常用字
   （GB2312 一级/二级 ≈ 6763 字）+ 平假名/片假名 + 全角标点。
   预算：≈ 7000 字 × 24B ≈ 168KB，加索引 < 250KB，`include_bytes!` 进 binary，
   对 `cargo install` 体积无感。
3. 缺字回退：渲染为 12×12 空心方框（tofu），滚动不中断。
4. Emoji：字体自带的单色 emoji 字形按全角 12×12 处理；不在字符集内的
   emoji 同样走 tofu。彩色 emoji、ZWJ 序列在 big 模式下**按 grapheme
   整体映射**：取该 cluster 的首个有字形的 codepoint，否则 tofu ——
   与主模式的 unicode-width 语义解耦（big 模式宽度 = 字形像素列数）。

生成物提交进仓库（build.rs 不参与，保证离线可构建、CI 无需下载字体）。

## 4. 渲染：半块字符（half-block）

终端 cell 高宽比约 2:1。用 Unicode 半块字符让 1 个 cell 表达 2 个纵向像素：

| 上像素 | 下像素 | 输出 |
|--------|--------|------|
| 亮 | 亮 | `█` |
| 亮 | 灭 | `▀` |
| 灭 | 亮 | `▄` |
| 灭 | 灭 | ` `（空格） |

- 单色前景（`--color` 沿用主模式选项；`--no-color` 时不输出 SGR）。
  V1 不需要 256/truecolor 双色 cell。
- 一个 12×12 全角字 → 12 列 × 6 行 cell；`--scale s` → 每像素放大为
  s×s cell：字面 12s 列 × 6s 行。scale 默认 1。终端尺寸限定（修订）：
  启动时要求 `cols ≥ 12s`（完整显示一个全角字）且 `rows ≥ 6s + 1`
  （字形行 + 1 行余量），不满足直接报错退出（exit 2，stderr 给出
  最小尺寸要求）；运行中 resize 跌破限定则 stderr 提示一次最小尺寸、
  降 scale 继续滚动，窗口恢复后 scale 回弹到请求值。
- 帧缓冲：`Vec<CellBuf>`（rows × width 的 enum 2bit 状态），每帧只重算
  viewport 覆盖的字形列；行间用 `cursor move to column 1 + 上一行`，
  整帧一次 `write + flush`（与主模式的低闪烁策略一致）。
- big 模式**同样不进 alternate screen**；退出时清掉自己占用的 N 行并
  恢复光标。Ctrl+C / panic 恢复逻辑复用 T-4 的 guard。

## 5. 滚动语义

- 横向滚动与主模式同构：位置以 **column** 计，engine 只需要
  `strip_width_columns = Σ glyph_cell_width(g) * scale` 与 viewport 宽度，
  逐帧求 `[x, x+term_cols)` 的可见切片。left/right/bounce/gap/repeat/once
  全部复用 T-5 的纯函数核心 —— big 模式只是换了
  “列 → 可见像素” 的着色器（glyph atlas 采样），不换运动学。
- 与 grapheme cluster 的对齐：主模式按 display width 滚动；big 模式
  按 cluster → 字形序列展开后的像素列滚动，切片边界落在字形内部时
  裁剪字形（左/右半字），保证视觉连续、不跳列。
- resize：宽变化 → 重算 viewport；高变化 → 按*请求的* scale 重新 clamp
  （缩小降级、恢复回弹）。位置换算沿用 T-5 的 resize 规则（按列比例保持相位）。
  跌破 §4 尺寸限定时不退出：stderr 提示一次最小尺寸要求，降级继续滚动。

## 6. 架构与模块

```
src/
├── bigfont/
│   ├── mod.rs        // BigFontFace: 资产加载、glyph 查找
│   ├── asset.rs      // 二进制格式解析（zero-copy, include_bytes!）
│   └── raster.rs     // glyph × scale → half-block 行缓冲
├── cli.rs            // --big, --scale <N>
├── marquee.rs        // Engine 增加 StripKind::BigFont（运动学不变）
├── renderer.rs       // BigRenderer: 多行帧缓冲、SGR、clear
└── ...
tools/gen-bigfont/    // BDF → assets/bigfont-*.bin（开发期工具，workspace member）
```

依赖：无新增运行时 crate（BDF 解析只在 tools 里，可用 bdf 解析 crate
或手写 ~150 行；binary 侧只有查表 + 位运算）。

## 7. CLI 变更

```
--big               启用超大字模式（fusion-pixel 12px 点阵）
--scale <N>         放大倍数，默认 1；终端不满足尺寸限定则报错退出
--font <NAME>       预留：zh-hans（默认）| ja | zh-hant（按打包资产）
```

与既有选项正交：`--speed/--fps/--direction/--bounce/--gap/--repeat/--once/
--no-color` 全部可用；`--align` 在 big 模式 V1 忽略（滚动模式下无意义）。

## 8. 测试策略

- asset.rs：格式 round-trip 测试（tools 生成小样本 → 解析 → 位图一致）。
- raster.rs：已知字形（如 “一” “口”）在 scale 1/2 下的 half-block 输出
  golden test；tofu 回退；半角/全角宽度。
- Engine：big 模式 strip 宽度、切片裁剪（字形被 viewport 边界切开）
  的纯函数测试，复用主模式测试骨架。
- 真实终端冒烟：`marquee "你好世界" --big --scale 2`（人工检查项）。

## 9. 风险与开放问题

1. **BDF 字符集覆盖**：zh-Hans 变体对日文汉字（如 「働」「込」）覆盖不全
   → 提供 ja 变体资产 + `--font ja`，或 V1 只做 zh-Hans 并在 README 说明。
2. **行高**：终端实际 cell 比例并非精确 2:1（字体相关），某些终端下
   大字会有轻微纵向拉伸感 —— 属于终端渲染固有现象，接受。
3. **tmux/SSH 带宽**：多行帧的 escape 序列量约为单行模式的 6×scale 倍，
   低速链路上需实测；必要时提供 `--scale 1` 降载建议写入 README。
4. 资产再生成流程要可复现：tools/gen-bigfont 记录字体 release tag + sha256。

## 10. 任务拆分（提交至 vuv P-1）

- T-9  `bigfont-asset-pipeline` — tools/gen-bigfont + 资产生成 + OFL 许可文件
  （依赖 T-2 unicode-cells：cluster→字形映射语义）
- T-10 `bigfont-raster` — asset.rs/raster.rs：加载、查表、half-block 光栅化
  （依赖 T-9）
- T-11 `bigfont-engine` — Engine 的 big-strip 运动学与切片裁剪
  （依赖 T-5 scroll-engine、T-10）
- T-12 `bigfont-renderer-cli` — 多行渲染器 + `--big/--scale/--font` + 集成冒烟
  （依赖 T-7 run-loop、T-11）
