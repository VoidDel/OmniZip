# 05 · 主题系统

## 1. 目标

- 浅色 / 深色 / 跟随系统三种模式，切换无闪烁、无需重启。
- 内置 ≥ 8 套主题，覆盖中性、暖色、冷色、高对比。
- 强调色可选预设、自定义或跟随系统（Windows / macOS / Linux 均支持读取系统强调色）。
- 任何人用一个 JSON 文件即可创建主题并热加载；Schema 校验 + 自动对比度校验。
- 主题覆盖所有可见表面：主窗口、对话框、菜单、轻量进度窗、预览窗、文件类型图标、Windows 右键菜单图标明暗。

## 2. 三层令牌模型

```
Layer 0  原始色板 primitives     neutral.0…1000  blue.50…950  …   （OKLCH 生成，每色 11 阶）
            │
Layer 1  语义令牌 semantic       bg.* / fg.* / border.* / accent.* / status.* / selection.*
            │                     ← 主题文件只需定义这一层
Layer 2  组件令牌 component      button.primary.bg  list.row.selected.bg  titlebar.bg …
                                  ← 默认由语义层派生，主题可按需覆盖
```

### 2.1 语义令牌清单（主题文件的必填与可选项）

| 分组 | 令牌 | 必填 | 派生规则（缺省时） |
|---|---|---|---|
| 背景 | `bg.canvas` `bg.surface` `bg.elevated` `bg.sidebar` | ✓ | — |
| | `bg.subtle` `bg.hover` `bg.pressed` | | `mix(bg.surface, fg.primary, 4% / 7% / 11%)` |
| 前景 | `fg.primary` `fg.secondary` | ✓ | — |
| | `fg.muted` `fg.disabled` `fg.on-accent` | | `alpha(fg.primary, 0.55 / 0.35)`；`on-accent` 取黑白中对比更高者 |
| 边框 | `border.subtle` `border.default` `border.strong` `border.focus` | `default` 必填 | `focus = accent` |
| 强调 | `accent` | ✓ | — |
| | `accent.hover` `accent.pressed` `accent.subtle` `accent.fg` | | OKLCH 明度 −6% / −12%；`subtle = alpha(accent, 0.14)` |
| 状态 | `success` `warning` `danger` `info`（各含 `.subtle`） | ✓ | `.subtle = alpha(x, 0.14)` |
| 选择 | `selection.bg` `selection.fg` | | `alpha(accent, 0.22)` / `fg.primary` |
| 阴影 | `shadow.1` `shadow.2` | | 深色主题自动加深 |
| 遮罩 | `overlay.scrim` | | `rgba(0,0,0,0.4)` |

非颜色令牌（主题可覆盖，通常不必）：`radius.sm/md/lg`、`space.1–8`、`font.ui`、`font.mono`、`size.base`、`density`、`motion.fast/normal/slow`。

### 2.2 组件令牌示例

| 组件令牌 | 默认值 |
|---|---|
| `titlebar.bg` | `bg.sidebar`（材质开启时半透明） |
| `commandbar.bg` | `bg.canvas` |
| `list.row.hover.bg` | `bg.hover` |
| `list.row.selected.bg` | `selection.bg` |
| `list.header.fg` | `fg.secondary` |
| `button.primary.bg` / `.fg` | `accent` / `accent.fg` |
| `progress.track` / `progress.fill` | `bg.subtle` / `accent` |
| `badge.encrypted.fg` | `warning` |
| `statusbar.bg` | `bg.surface` |

## 3. 主题文件格式

```json
{
  "$schema": "https://omnizip.app/schemas/theme/1.json",
  "schemaVersion": 1,
  "id": "midnight",
  "name": { "en-US": "Midnight", "zh-CN": "午夜" },
  "author": "OmniZip",
  "appearance": "dark",
  "colors": {
    "bg.canvas": "#0F1115",
    "bg.surface": "#161922",
    "bg.elevated": "#1C2029",
    "bg.sidebar": "#12151C",
    "fg.primary": "#E6E8EE",
    "fg.secondary": "#A3A9B7",
    "border.default": "#2A3040",
    "accent": "#5B8CFF",
    "success": "#3DD68C",
    "warning": "#F5B544",
    "danger": "#FF6B6B",
    "info": "#58B7FF"
  },
  "components": {
    "titlebar.bg": "#0C0E12"
  },
  "shape": { "radius.md": 8 },
  "effects": { "material": "auto", "shadow": "soft" },
  "icons": { "fileIconTheme": "duotone", "typeColors": "muted" }
}
```

- `colors` 中只需填写必填项，其余自动派生；填写了即覆盖。
- `effects.material`：`auto` | `mica` | `acrylic` | `vibrancy` | `none`。
- 打包格式 `.omnitheme`：ZIP 容器，内含 `theme.json` 与可选 `icons/`（自定义文件图标包）。

## 4. 模式解析与系统联动

### 4.1 外观模式

- `appearance = light | dark | system`。
- 用户为浅色与深色各选一个主题（"主题对"），`system` 模式在二者间切换。
- 系统检测：Tauri `Window::theme()` + `onThemeChanged`；Linux 额外监听 `org.freedesktop.portal.Settings` 的 `color-scheme`。
- 可选定时切换（日出 / 日落或固定时间段）。
- Windows 系统开启"高对比"时自动切到高对比主题并禁用材质；关闭后恢复。

### 4.2 强调色

| 来源 | 实现 |
|---|---|
| 主题默认 | 主题文件 `accent` |
| 跟随系统 | Windows：WinRT `UISettings.GetColorValue(UIColorType::Accent)`，监听 `ColorValuesChanged`；macOS：`NSColor.controlAccentColor`，监听 `AppleColorPreferencesChangedNotification`；Linux：portal `accent-color`（xdg-desktop-portal ≥ 1.17，GNOME 47+ / KDE 6+），不可用时回退主题默认 |
| 自定义 | 颜色选择器 + 预设 12 色 |

派生：强调色转换到 OKLCH 后生成 hover / pressed / subtle / fg，保证在任何主题上都有一致的交互反馈。

### 4.3 对比度守卫

主题解析完成后自动校验：

| 组合 | 最低对比度 |
|---|---|
| `fg.primary` / `bg.canvas`、`bg.surface`、`bg.elevated` | 4.5 : 1 |
| `fg.secondary` / 同上 | 3.0 : 1（目标 4.5） |
| `accent.fg` / `accent` | 4.5 : 1 |
| `border.default` / `bg.surface` | 1.5 : 1 |
| 状态色 / `bg.surface` | 3.0 : 1 |

不达标时在 OKLCH 空间逐步调整明度直至通过，并在主题编辑器中显示告警；内置主题在 CI 中强制通过。

## 5. 内置主题

| id | 名称 | 外观 | 特点 | 许可 |
|---|---|---|---|---|
| `aurora` | 晨光 | 浅 | **默认浅色**，冷白底、蓝强调 | 自研 |
| `midnight` | 午夜 | 深 | **默认深色**，蓝黑底 | 自研 |
| `graphite` | 石墨 | 深 | 纯中性灰，含 OLED 纯黑变体 | 自研 |
| `paper` | 纸张 | 浅 | 暖白底、琥珀强调，适合长时间阅读 | 自研 |
| `nord` / `nord-light` | 北极 | 深 / 浅 | Nord 调色板 | MIT |
| `catppuccin-mocha` / `catppuccin-latte` | 卡布奇诺 | 深 / 浅 | Catppuccin 调色板 | MIT |
| `solarized-dark` / `solarized-light` | 日光 | 深 / 浅 | Solarized 调色板 | MIT |
| `high-contrast-dark` / `high-contrast-light` | 高对比 | 深 / 浅 | 纯色、无透明、2 px 边框、加粗焦点环 | 自研 |

## 6. 窗口材质

| 平台 | 支持 | 应用位置 |
|---|---|---|
| Windows 11 22H2+ | Mica（侧栏 / 标题栏）、Mica Alt、Acrylic（菜单、浮层） | `window-vibrancy` crate |
| Windows 10 | Acrylic（默认关，性能一般） | 同上 |
| macOS | `NSVisualEffectView`：侧栏 `.sidebar`、菜单 `.menu`、浮层 `.popover` | 同上 |
| Linux | 不支持，使用纯色 | — |

规则：材质开启时 `bg.sidebar` / `titlebar.bg` 以半透明渲染；高对比、省电模式、远程桌面会话、窗口失焦（可选）下自动关闭材质并回退到纯色令牌。

Windows 11 上材质可在设置中选择 Mica（只取桌面壁纸、模糊最重）、Mica Alt（壁纸色调更浓）或亚克力（实时模糊窗口后方的一切，包括其他窗口），macOS 固定为毛玻璃。进度小窗与右键菜单打开的独立对话框窗口同样使用所选材质，整窗以主题底色 55%（深色 60%）的薄层覆盖以保证文字可读；材质窗口在 Windows 上不会自动把键盘焦点交给 WebView，显示或弹出询问时需显式聚焦页面。

实现要点：材质开启时侧栏以主题侧栏色的 35%（深色主题 45%）不透明度覆盖在 Mica 之上，既保留主题色调，又能透出壁纸色调；不同主题效果差异较大，晨光主题的侧栏色本就接近浅色 Mica，变化很轻微，纸张、午夜等主题变化明显。支持材质的系统上主窗口背景始终设为完全透明，材质关闭时由页面自身绘制主题底色：WebView 的不透明底色会画在页面下方、把 Mica 完全盖住，而且运行时无法可靠地改回透明。

## 7. 文件类型图标主题

- 内置两套：`mono`（单色线性，颜色取 `fg.secondary`）与 `duotone`（双色，类型色 + 中性）。
- SVG 使用 `currentColor` 与 CSS 变量 `--icon-type-color` 驱动，类型色强度由主题 `icons.typeColors = vivid | muted | none` 控制。
- 第三方图标包：目录 + `manifest.json`（`ext → svg`、`mime → svg`、`folder` / `folder-open` / `archive` 特殊项）。
- 格式徽章（ZIP / 7Z / RAR 等）颜色固定（品牌识别），仅明度随主题调整。

## 8. 实现要点

1. **ThemeService（Rust）**：扫描内置目录与用户目录 → JSON Schema 校验（`jsonschema` crate）→ 解析派生 → 对比度守卫 → 生成 `ResolvedTheme`（完整令牌表）→ 通过事件推送前端；监视用户目录实现热重载。
2. **前端应用**：把令牌写入 `document.documentElement.style` 的 CSS 变量；设置 `data-appearance="dark|light"` 与 `color-scheme`，让原生滚动条、表单控件、`<select>` 跟随。
3. **防首屏闪白**：Rust 启动时先读配置得到上次 `bg.canvas`，用它设置窗口 `backgroundColor`；窗口 `visible: false` 创建，前端首帧渲染后再 `show()`；关键令牌内联在 `index.html` 中。
4. **切换过渡**：支持 View Transitions API 时做 200 ms 交叉淡入；否则直接切换；`prefers-reduced-motion` 下禁用。
5. **多窗口同步**：ThemeService 为唯一真相源，所有窗口订阅同一事件。
6. **Shell 扩展**：Windows 右键菜单图标提供浅 / 深两套资源，依据 `HKCU\...\Themes\Personalize\AppsUseLightTheme` 选择。
7. **密度**：`density = compact | comfortable | spacious` 映射到 `--row-height`、`--control-height`、`--space-*` 缩放系数。
8. **字体缩放**：`size.base` 可在 12–20 间调整，所有尺寸用 `rem` 表达。

## 9. 自定义主题

- 存放目录：`config_dir()/themes/`；设置页"外观 → 主题 → 打开主题文件夹 / 导入 / 导出"。
- 导入 `.omnitheme` 或 `.json`；导出时附带当前强调色与密度（可选）。
- **主题编辑器**（v1.1）：左侧按分组列出语义令牌（颜色选择器 + 派生提示），右侧实时预览（主窗缩略、对话框、菜单、进度窗）；对比度实时告警；"从当前主题派生"一键创建副本。
- `schemaVersion` 升级时自动迁移并备份旧文件。

## 10. 质量保障

- stylelint 规则：禁止裸色值、禁止非令牌 `border-radius` / `padding` / `margin` / `box-shadow`。
- 视觉回归：Playwright 截图矩阵 = 每内置主题 × 浅/深 × 3 语言 × 8 个关键界面；差异阈值 0.1%。
- 对比度自动化测试覆盖全部内置主题。
- 性能：主题切换导致的布局重算 ≤ 1 帧；CSS 变量总数 < 300。
