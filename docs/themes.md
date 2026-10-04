# marquee 主题大全

`--theme NAME` 从下表选一个；自己的主题写在
`~/.config/marquee/themes.toml`（或 `--theme-file PATH` 指定），同名覆盖
内置主题。语法细节见[手册](manual.md)。

调色盘（颜色列表）沿屏幕列铺色带，文字从色带中流过；`band = N` 设定每色
占几列。配图用 [vhs](https://github.com/charmbracelet/vhs) 录制，tape 在
[`tools/vhs/`](../tools/vhs/)。

## 纯色主题（7 个）

| 主题 | 效果 | 等价 TOML |
|---|---|---|
| `default` | 无色，即经典单色跑马灯（默认） | ——（空主题） |
| `bold` | 加粗，默认前景色 | `bold = true` |
| `alarm` | 加粗亮红 | `fg = "bright-red"` + `bold = true` |
| `gold` | 加粗亮黄 | `fg = "bright-yellow"` + `bold = true` |
| `matrix` | 加粗亮绿 | `fg = "bright-green"` + `bold = true` |
| `ice` | 亮青 | `fg = "bright-cyan"` |
| `violet` | 加粗亮紫 | `fg = "bright-magenta"` + `bold = true` |

大字体模式配图（`alarm` 加粗亮红）：

```sh
marquee --big --scale 2 "ON AIR" --theme alarm
```

![alarm 主题（大字体）](images/big-font.gif)

`matrix`（加粗亮绿）：

```sh
marquee "好消息 · marquee v1 正式发布" --theme matrix
```

![matrix 主题](images/theme-matrix.gif)

## 调色盘主题（4 个）

颜色列表沿屏幕列循环铺色带——字流从色带中穿过，闪闪发光。

### `rainbow` —— 经典六色彩虹

```sh
marquee --continuous " ON AIR " --theme rainbow
```

![rainbow 主题](images/theme-rainbow.gif)

```toml
[rainbow]
fg = ["bright-red", "bright-yellow", "bright-green", "bright-cyan", "bright-blue", "bright-magenta"]
```

连续滚动 + rainbow 的完整效果：

![rainbow 连续滚动](images/continuous.gif)

### `sunset` —— 落日暖色

```sh
marquee "sunset · 落日色带 warm bands" --theme sunset
```

![sunset 主题](images/theme-sunset.gif)

```toml
[sunset]
fg = ["#ff5e62", "#ff9966", "#ffd194"]
```

### `ocean` —— 海洋冷色

```toml
[ocean]
fg = ["#00c3ff", "#0072ff", "#00e5c0"]
```

### `neon` —— 霓虹之夜

黑底上三色霓虹，赛博感：

```sh
marquee "NEON · 霓虹之夜 night glow" --theme neon
```

![neon 主题](images/theme-neon.gif)

```toml
[neon]
fg = ["#ff2975", "#00f3ff", "#8f5aff"]
bg = "black"
```

## 自定义你的主题

```toml
# ~/.config/marquee/themes.toml
[my-theme]
fg = ["#ff0051", "#ff8f00", "#ffe600", "#00e650", "#00c3ff", "#8f5aff"]  # 调色盘
bg = "black"            # 背景也支持列表（空白列也铺色带）
band = 3                # 每色占 3 列（默认 1）
bold = true             # 还有 dim / italic / underline
```

- 颜色写法：16 个 ANSI 颜色名（`red`、`bright-green`、`bright red` 均可）
  或 `#rrggbb` / `#rgb`
- `fg`/`bg` 单值 = 纯色；两个以上 = 调色盘
- 列表至少一个颜色；`band` 至少 1 列
- 主题名撞内置主题则覆盖之；`--theme 不存在的名字` 会列出全部可用主题

[← 返回 README](../README.md)
