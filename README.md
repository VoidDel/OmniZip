# OmniZip 设计方案

> 一款面向 Windows / macOS / Linux 的现代化压缩与解压软件：统一的 Rust 核心、美观大气的界面、多语言、多主题、广泛的格式支持、深度的 Shell 集成（右键菜单 + 文件关联 + 命令行），以及把任意压缩包像 ISO 一样直接挂载为虚拟磁盘的能力。

本仓库已按路线图进入实施，当前为 **M1 MVP（Windows 优先）**：核心库、ZIP / 7z / TAR 系列 / RAR 引擎、`omnizip` 命令行、Tauri 桌面应用、Windows 右键菜单扩展、挂载核心与 Windows 安装包均已可构建并通过测试。Win11 一级右键菜单已用自签证书的 MSIX 实机验证；Windows 上已可用 WinFsp 把压缩包挂载为盘符或文件夹（可带临时可写层，卸载时保存回压缩包或放弃），由挂载守护进程保持、OmniZip 退出后仍在；ZIP / gzip 多线程压缩、大量小文件的解压与 .xz 解码已优化，与 Bandizip 的对比见 [11 §8.4](docs/11-roadmap.md#84-性能与正确性2026-10-29)；正式代码签名、macOS 与 Linux 尚未完成。逐项进度见 [11 §8 实施状态](docs/11-roadmap.md#8-实施状态2026-10-29windows-优先)，设计文档位于 [docs/](docs/)。

## 一句话定位

把 7-Zip 的格式覆盖与性能、Bandizip 的易用性与中文乱码处理、Keka 的 macOS 体验，用一套跨平台代码库做到三大桌面系统上，并且界面达到现代原生应用水准。

## 文档索引

| 编号 | 文档 | 内容 |
|---|---|---|
| 01 | [产品概述与目标](docs/01-overview.md) | 定位、用户、平台矩阵、需求映射、非目标、成功指标、开放问题 |
| 02 | [技术架构](docs/02-architecture.md) | 技术选型对比与决策、分层架构、进程模型、核心库抽象、性能预算、风险 |
| 03 | [压缩格式支持](docs/03-formats.md) | 格式分级、引擎选择、能力矩阵、压缩/解压参数模型、文件名编码修复 |
| 04 | [界面与交互设计](docs/04-ui-design.md) | 设计原则、视觉语言、窗口布局、关键界面、交互细节、可访问性、平台差异 |
| 05 | [主题系统](docs/05-theming.md) | 三层设计令牌、明暗/高对比、内置主题、系统强调色与窗口材质、自定义主题 |
| 06 | [国际化](docs/06-i18n.md) | 语言清单、资源格式、ICU 规则、Rust 侧本地化、RTL、翻译流程、排序与格式化 |
| 07 | [Shell 集成](docs/07-shell-integration.md) | Windows/macOS/Linux 右键菜单、文件关联、默认程序、单实例与参数转发 |
| 08 | [命令行工具](docs/08-cli.md) | `omnizip` CLI 命令集、输出格式、退出码、管道、补全与手册页 |
| 09 | [安全设计](docs/09-security.md) | 不可信输入处理、Zip Slip、压缩炸弹、密码、沙箱、供应链 |
| 10 | [工程、构建与发布](docs/10-build-release.md) | 仓库结构、工具链、CI、打包、签名、自动更新、测试策略 |
| 11 | [路线图](docs/11-roadmap.md) | 里程碑 M0–M4、范围与验收标准 |
| 12 | [虚拟挂载](docs/12-mount.md) | 把任意压缩包挂载为虚拟磁盘：三平台后端、按格式的随机访问策略、守护进程、缓存、安全、CLI |

## 关键决策速览

| 主题 | 决策 |
|---|---|
| 许可证 / 名称 | **MIT** 开源；名称暂定 OmniZip，品牌字符串集中配置便于后续更换 |
| 技术栈 | Tauri 2 + Rust（核心 / CLI / Shell 扩展）+ React 19 + TypeScript（界面） |
| 格式引擎 | 纯 Rust 引擎优先（zip / sevenz-rust2 / tar / flate2 / xz / zstd / lz4 / brotli），RAR 通过隔离的 unrar 只读，长尾格式用可选的 libarchive 桥接 |
| 界面 | 自绘标题栏、8pt 网格、虚拟化列表、Rust 侧条目索引 + 前端分页，Win11 Mica / macOS Vibrancy |
| 主题 | 原始色板 → 语义令牌 → 组件令牌三层；JSON 主题文件 + Schema 校验；跟随系统强调色 |
| 多语言 | 前端 i18next + ICU MessageFormat；Rust 侧 Fluent；核心库只返回错误码，不返回文案 |
| Shell | Windows：Rust 编写的 COM `IExplorerCommand` 扩展 + MSIX 包身份；macOS：Services + Finder 扩展；Linux：Dolphin/Thunar/Nemo 声明式菜单 + nautilus-python |
| 挂载 | 独立守护进程 `omnizip-mountd`；Windows WinFsp 盘符、macOS FSKit（NFS 回环保底）、Linux FUSE3；按格式分级的随机访问与缓存策略，内容按需解压不落盘整包 |
| 安全 | 全部输入视为恶意；路径净化、压缩炸弹限额、MOTW 传播、C 引擎进程隔离（v2） |

## 交互原型

[prototype/index.html](prototype/index.html) 是基于 04（界面）与 05（主题）规范实现的可交互高保真原型，用浏览器直接打开即可。它覆盖开始页、压缩包浏览器、新建压缩包 / 解压 / 挂载对话框、任务中心、设置页，内置 12 套主题、4 种语言，并可切换 Windows / macOS / Linux 三种窗口样式。原型中的数据与操作均为模拟。

## 建议阅读顺序

1. 先读 01、02 建立整体认知。
2. 产品/设计同学重点读 04、05、06。
3. 工程同学重点读 03、07、08、09、10、12。
4. 排期看 11。

## 构建与运行

前置条件：Rust stable（≥ 1.97）、Node 22+、pnpm 10（`corepack enable` 或 `npx pnpm@10`）。Windows 另需 Visual Studio 2022 C++ 生成工具与 WebView2；macOS 另需 Xcode 命令行工具；Linux 另需 `libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev`。

```sh
pnpm install
pnpm i18n-check                  # 四种语言的键是否齐全
cargo test --workspace           # 核心、引擎、CLI、挂载核心、Shell 扩展、桌面应用
pnpm test                        # 前端单元测试
cargo run -p omnizip-cli -- --help
pnpm dev                         # 开发模式运行桌面应用（先构建挂载守护进程 omnizip-mountd 到 target/debug）
pnpm bundle:windows              # NSIS 安装包（含 CLI、右键菜单 DLL 与 Win11 菜单包，需 Windows SDK）→ target/release/bundle/nsis/
pwsh platform/windows/msix/pack.ps1 -Version 0.1.0   # 完整 MSIX（商店渠道，签名可选）
pnpm build                       # 在 Mac 上：OmniZip.app 与 .dmg（未签名，含文件关联与文档图标）→ target/release/bundle/
```

挂载守护进程在有挂载时一直运行，新构建（或更新安装后）的守护进程要等全部挂载卸载、旧进程退出（无挂载 5 分钟后）才会启用。

模糊测试在 nightly 上运行：`cargo +nightly fuzz run zip`（目标见 `fuzz/fuzz_targets/`）；稳定版可运行其冒烟测试 `cargo test --manifest-path fuzz/Cargo.toml --lib`。

### Win11 一级右键菜单与测试证书

程序以 EXE（NSIS）安装；Win11 一级右键菜单需要包身份，由安装包附带的菜单包 `omnizip_menu.msix`（Sparse Package，只声明菜单，文件来自安装目录）在安装时注册，做法同 Bandizip。系统只注册受信任证书签名的包，正式证书到位前可以用自签证书在本机或测试机上验证；这类安装包不能对外分发。

```powershell
# 生成测试证书（私钥存放在 %LOCALAPPDATA%\OmniZip\dev-cert，不进仓库）
# -Trust 把公钥证书导入“本地计算机\受信任的人”，需要管理员权限，会弹出 UAC
pwsh platform/windows/msix/new-test-cert.ps1 -Trust
pnpm bundle:windows              # 发现测试证书时自动用它给菜单包签名
```

测试证书的公钥在仓库中（`platform/windows/msix/omnizip-test.cer`），私钥 `.pfx` 只保存在维护者电脑上（`%LOCALAPPDATA%\OmniZip\dev-cert`），不进仓库。程序内置该公钥：在其他电脑上安装时，若该证书尚未受信任，安装程序会弹出一次 UAC，确认后自动导入“本地计算机\受信任的人”再注册菜单；拒绝时安装照常完成，只有经典菜单（显示更多选项），之后可在设置页重新注册。导入后，该电脑信任所有用这把测试私钥签名的包，因此 `.pfx` 务必保密。完整 MSIX 与菜单包不要同时安装，否则一级菜单出现两次：`Get-AppxPackage OmniZip.OmniZip | Remove-AppxPackage` 卸载完整 MSIX。

## 仓库结构

```
OmniZip/
├─ Cargo.toml                 # Rust workspace（resolver 3，edition 2024）
├─ package.json               # pnpm workspace
├─ crates/
│  ├─ omnizip-core/           # 抽象、格式检测、路径净化、编码修复、TreeIndex、作业
│  ├─ omnizip-engine-zip/     # ZIP（zip crate）
│  ├─ omnizip-engine-7z/      # 7z（sevenz-rust2）
│  ├─ omnizip-engine-tar/     # TAR 与 gz / bz2 / xz / zst / lz4 / br
│  ├─ omnizip-engine-rar/     # RAR 只读（unrar）
│  ├─ omnizip-formats/        # 引擎注册表、格式转换
│  ├─ omnizip-i18n/           # Fluent 本地化（CLI、Shell、错误码）
│  ├─ omnizip-cli/            # bin: omnizip
│  ├─ omnizip-vfs/            # 挂载核心：VirtualFs、ArchiveFs、内容缓存、后端探测
│  ├─ omnizip-mount-winfsp/   # Windows 挂载后端（运行时加载 WinFsp）
│  └─ omnizip-shell-win/      # cdylib: omnizip_shell.dll（IExplorerCommand）
├─ apps/desktop/              # Tauri 2 应用（src-tauri + React 前端）
├─ icons/                     # 应用图标源图（app-icon.svg）；filetypes/ 压缩文件类型图标（scripts/gen-filetype-icons.mjs 生成）
├─ platform/windows/          # nsis/hooks.nsh；msix/AppxManifest.xml（完整 MSIX）、AppxManifest.sparse.xml（菜单包）与 pack.ps1
├─ locales/                   # en-US、zh-CN、zh-TW、ja-JP
├─ themes/                    # 12 套内置主题 JSON 与 Schema
├─ scripts/                   # i18n-check、gen-builtin-themes、prepare-bundle
├─ fixtures/                  # 测试用 RAR 样本
├─ fuzz/                      # cargo-fuzz 目标（独立 workspace）
├─ prototype/                 # 交互原型
├─ docs/                      # 设计文档
└─ .github/workflows/         # ci、release、nightly-fuzz
```

详细规划见 [10-build-release.md](docs/10-build-release.md)。
