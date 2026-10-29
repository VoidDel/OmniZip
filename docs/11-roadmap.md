# 11 · 路线图

## 1. 总览

| 阶段 | 周期（建议） | 目标 |
|---|---|---|
| M0 技术验证 | 3 周 | 把风险最高的点全部做成 PoC，决定剩余 ADR |
| M1 MVP | 6 周 | Windows 优先、日常可用的内部版本 |
| M2 三平台功能完整 | 8 周 | 格式、挂载、Shell、CLI 三平台全覆盖 |
| M3 打磨与 1.0 发布 | 6 周 | 主题与语言完整、自动更新、渠道上架 |
| M4 / v1.x | 持续 | 可写挂载、主题编辑器、沙箱、插件 |

建议团队：1 名 Rust 核心、1 名 Rust 平台（Shell / 挂载）、1 名前端、0.5 名设计、0.5 名 QA / DevOps；macOS Swift 扩展可由平台工程师兼顾或短期外协。

## 2. M0 · 技术验证（3 周）

| # | 验证项 | 通过标准 |
|---|---|---|
| 1 | Tauri 2 壳：自绘标题栏、Mica、Snap Layout、防闪白 | 冷启动 < 1 s；三平台截图一致 |
| 2 | 百万条目虚拟列表：Rust `TreeIndex` + 前端分页 | 滚动 60 fps；JS 内存恒定 |
| 3 | ZIP / 7z / tar.zst 读写 round-trip | 与 7-Zip 互操作；7z 多线程 LZMA2 速度 ≥ 7-Zip 的 70%（M0 门槛，1.0 目标 90%） |
| 4 | Win11 现代右键菜单：Rust `IExplorerCommand` + Sparse Package | Win11 24H2 一级菜单出现；ARM64 可加载 |
| 5 | 挂载：WinFsp 盘符挂载 ZIP 并播放 MP4；Linux `fuser`；macOS FSKit 与 NFS 回环对比 | 三平台能在文件管理器打开并读取；决定 macOS 主后端 |
| 6 | 文件名编码探测 | 200 个样本准确率 > 95% |
| 7 | 主题令牌管线：JSON → Schema 校验 → CSS 变量热切换 | 切换无闪烁；对比度守卫生效 |
| 8 | Shell 扩展稳定性 | AppVerifier 下 1000 次右键无错误 |

退出标准：全部通过，或有书面替代方案并更新 ADR；完成 FSKit 实测并确定 macOS 挂载后端。许可证已定为 MIT，名称暂定 OmniZip。

## 3. M1 · MVP（6 周）

范围：
- 格式：ZIP、7z、TAR.*、单文件 gz / xz / zst 读写；RAR 只读。
- 界面：开始页、压缩包浏览器（表格视图、搜索、排序、多选）、压缩与解压对话框、任务中心、密码与冲突对话框、文本 / 图片预览。
- 主题：晨光 / 午夜 + 跟随系统 + 系统强调色；多语言：zh-CN、en-US。
- Shell：Windows 现代 + 经典右键菜单、文件关联、轻量进度窗、单实例转发。
- CLI：`x e a l t i formats completions`。
- 挂载：Windows WinFsp 只读挂载（ZIP / 7z 非固实 / tar）。
- 打包：Windows MSIX 与 NSIS（未上架）。

验收：内部用户连续一周用它替代原有压缩软件；崩溃率为零；性能预算达标。

## 4. M2 · 三平台功能完整（8 周）

- 格式：Tier 2 全部只读；多卷；文件名加密；ZIP 原地更新；格式转换；SFX 识别。
- 挂载：三平台后端、守护进程、固实块缓存、tar.gz 索引、侧栏管理、CLI `mount` 系列。
- Shell：macOS Services + Finder 扩展 + UTI；Linux 全部文件管理器菜单 + MIME；MOTW / quarantine 传播。
- 界面：网格视图、拖出解压、预览窗完整、设置全部分组、可访问性审计。
- 主题：全部内置主题、材质、自定义主题导入导出、密度。
- 多语言：zh-TW、ja-JP；i18n 工具链与翻译平台接入。
- CLI：全部命令、`--json` 契约、手册页、补全。
- 安全：路径净化测试集、炸弹限制、fuzz 流水线、`cargo-deny`。
- 打包：DMG、.deb / .rpm / AppImage、Flatpak manifest。

验收：三平台手测矩阵通过；差分测试全绿；外部 beta 50 人。

## 5. M3 · 打磨与 1.0（6 周）

- 多语言扩展到 10 种；字符串冻结与审校。
- 自动更新、签名、公证、渠道上架（winget、Homebrew、Flathub、AUR）。
- 性能冲刺：7z 写入 ≥ 90%、启动、内存。
- 第三方安全审计与修复。
- 文档站、帮助、快捷键表、诊断包。
- 开源治理（若开源）：CONTRIBUTING、CLA、issue 模板。

验收：1.0 发布清单全部勾选。

## 6. M4 · v1.x 待办（按价值排序）

1. ZIP 可写挂载（overlay）
2. 主题编辑器
3. 沙箱 Worker 进程承载 C 引擎
4. SFX 自解压创建（Windows）
5. App Intents / 快捷指令（macOS）、Jump List 任务扩展（Windows）
6. Quick Look 预览扩展（macOS）、`IPreviewHandler`（Windows）
7. 插件 API（WASM 组件模型：第三方格式、预览器、后处理）
8. 压缩包修复与恢复记录（PAR2 集成）
9. 两个压缩包差异比较、重复文件检测
10. 云存储（WebDAV / S3）直接浏览与挂载
11. 增量更新（bsdiff）
12. App Store 版本（Sandbox）

## 7. 主要风险的时间线

| 风险 | 暴露阶段 | 应对 |
|---|---|---|
| 7z 写入性能不达标 | M0 | FFI LZMA SDK 作为备选引擎 |
| FSKit 对虚拟资源支持不足 | M0 | NFS 回环为主后端，FSKit 延后 |
| WinFsp 许可与分发 | 已解决 | MIT 开源符合 WinFsp 的 FLOSS 例外 |
| Win11 菜单在无身份渠道不可见 | M1 | 便携版说明 + 一键注册 Sparse Package |
| 翻译质量 | M3 | 提前 4 周接入平台，审校者到位 |
| macOS 公证 / 扩展审核问题 | M2 | 提前用开发者账号跑通全链路 |

## 8. 实施状态（2026-10-27，Windows 优先）

本节记录按路线图实施 M0 / M1 的实际进度。"已验证"指有自动化测试或在 Windows 11 上实际运行并截图核对；macOS 只做过开发运行与文件关联的实机检查（见 07 §4.2）；未在 Linux 上构建或运行过。

### 8.1 M0 技术验证

| # | 验证项 | 状态 |
|---|---|---|
| 1 | Tauri 2 壳 | 已实现自绘标题栏、Mica、窗口初始隐藏防闪白、窗口状态记忆（Windows 已验证）。未做：标题栏最大化按钮的 Snap Layout 悬停菜单、冷启动计时、三平台截图对比 |
| 2 | 百万条目虚拟列表 | 已实现 Rust `TreeIndex` 分页 + 前端 TanStack Virtual。未做：百万条目下的帧率与内存测量 |
| 3 | ZIP / 7z / tar.zst round-trip | 已验证（引擎与注册表测试覆盖 ZIP、7z、TAR、tar.gz / xz / zst）。未做：与 7-Zip 的互操作差分与速度对比 |
| 4 | Win11 现代右键菜单 | 已验证（2026-10-27）：用自签测试证书签名并安装 MSIX 后，Win11 一级菜单出现 OmniZip 子菜单（压缩包上显示打开、解压到此处、解压到"名称\"、解压到…、挂载、测试），"解压到此处"端到端解压成功；COM 类可从未打包进程激活；`omnizip` 执行别名可用。未做：ARM64 构建、Sparse Package（NSIS 渠道的一级菜单） |
| 5 | 挂载 | 已验证（2026-10-29）：挂载核心 `omnizip-vfs` 与 WinFsp 后端 `omnizip-mount-winfsp`，经真实驱动挂载 ZIP / 固实 7z 到盘符与文件夹（见 12 §13）。Linux `fuser` 与 macOS FSKit / NFS 对比未开始 |
| 6 | 文件名编码探测 | 已实现（chardetng + 区域提示），有单元测试。未做：200 样本准确率评测 |
| 7 | 主题令牌管线 | 已验证：JSON 主题 → 校验 → CSS 变量热切换，对比度守卫，12 套内置主题 |
| 8 | Shell 扩展稳定性 | 引用计数与 `DllCanUnloadNow` 有测试。未做：AppVerifier 下 1000 次右键 |

### 8.2 M1 MVP

| 范围 | 状态 |
|---|---|
| 格式 | 已完成且超出范围：ZIP、7z、TAR 及 gz / bz2 / xz / zst / lz4 / br 组合与单文件读写，RAR 只读，ZIP 原地添加 / 删除 / 重命名，格式转换 |
| 界面 | 已完成：开始页、浏览器（表格、搜索、排序、多选）、压缩 / 解压对话框、任务中心、密码与冲突对话框、文本 / 图片 / 十六进制预览 |
| 主题与语言 | 已完成且超出范围：12 套内置主题、跟随系统、系统强调色；zh-CN、en-US、zh-TW、ja-JP 四种语言 |
| Shell | 已完成：经典右键菜单（设置页一键注册 / 移除）、轻量进度窗、单实例转发。安装器钩子自动注册与文件关联已写入安装包，但未实际安装验证。Win11 一级菜单已用自签证书的 MSIX 实机验证。macOS：文件关联与每格式文档图标、Finder 打开文件已实机验证，右键菜单（Services、Finder 扩展）未做 |
| CLI | 已完成：`x e a l t i formats completions`，另有 `d rn convert gui` |
| 挂载 | Windows 已完成：WinFsp 只读挂载、挂载对话框、侧栏与设置页的已挂载列表、退出时卸载 / 启动时恢复。挂载暂由桌面应用托管，守护进程 `omnizip-mountd` 与 CLI `mount` 未做 |
| 打包 | NSIS 安装包已产出（含 CLI、Shell DLL、安装 / 卸载钩子）；MSIX 可用自签测试证书签名并安装（`platform/windows/msix/new-test-cert.ps1`），正式签名待证书；macOS 可产出未签名的 `.app` 与 `.dmg`（本机架构） |
| CI | 已编写 `ci`、`release`、`nightly-fuzz` 工作流；仓库尚未推送，工作流未实际运行 |
| 验收 | 未开始：内部一周试用、崩溃率统计、性能预算全面测量 |

### 8.3 后续优先级

1. 挂载守护进程 `omnizip-mountd` 与 CLI `mount` / `unmount` / `mounts`；安装包中的 WinFsp 可选组件。
2. 申请正式代码签名证书（自签证书已验证 Win11 一级菜单）；补 ARM64 DLL 与 Sparse Package。
3. 推送仓库，让 CI 首次在 Windows / macOS / Linux 运行并修复平台差异。
4. 完成 M0 未做的测量项：百万条目列表、与 7-Zip 的速度与互操作、编码探测准确率、AppVerifier。
