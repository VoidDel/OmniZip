# 02 · 技术架构

## 1. 技术选型

### 1.1 候选方案对比

| 维度 | Tauri 2 + Rust | Qt 6（C++/QML） | Electron | Avalonia（.NET） | Flutter Desktop |
|---|---|---|---|---|---|
| 安装包体积 | 小（10–25 MB） | 中（30–60 MB） | 大（150 MB+） | 中（40–80 MB） | 中（30–50 MB） |
| 空闲内存 | 低–中（系统 WebView） | 低 | 高 | 中 | 中 |
| 压缩库生态 | Rust 生态成熟，内存安全 | C 生态最全，可直接链 7-Zip 源码 | 需 native addon | 需 P/Invoke | 需 FFI |
| 主题 / 美观自由度 | 最高（CSS） | 中（QML 样式成本高） | 最高 | 高 | 高 |
| i18n | 成熟（i18next / ICU） | Qt Linguist | 成熟 | 成熟 | 成熟 |
| 系统集成（COM / Cocoa） | Rust 直接调用 | 最成熟 | 需 native 模块 | 一般 | 弱 |
| 处理不可信输入的安全性 | Rust 内存安全 | 需格外小心 | 取决于 addon | 托管安全 | 取决于 FFI |
| 许可证 | MIT / Apache-2.0 | LGPL / 商业 | MIT | MIT | BSD |
| 主要风险 | Linux WebKitGTK 一致性；大列表需虚拟化 | 团队 C++ 门槛；界面现代化成本 | 体积 / 内存口碑 | 社区规模 | 桌面成熟度与系统集成 |

### 1.2 决策

**主选：Tauri 2 + Rust（核心库、应用层、CLI、Windows Shell 扩展）+ React 19 + TypeScript（界面）。**

理由：
1. 压缩软件的核心风险是解析不可信输入。Rust 从语言层消除整类内存安全漏洞，这对加载进 `explorer.exe` 的 Shell 扩展尤其重要。
2. Rust 压缩生态可覆盖主要格式；长尾格式用 libarchive FFI 补齐，且可按 feature 关闭。
3. Web 前端让"多主题 + 多语言 + 大气界面"的实现成本最低，设计稿到实现几乎无损。
4. 一份核心库同时服务 GUI、CLI、Shell 扩展，行为完全一致。
5. 体积与内存远优于 Electron，接近原生。

**备选**：若团队 C++ 深厚，可用 Qt 6 Widgets/QML 做界面、Rust 做核心（通过 cxx-qt 桥接）。本方案其余部分（核心库、格式、Shell、CLI、安全）与界面技术无关，可直接复用。

## 2. 分层架构

```
┌────────────────────────────────────────────────────────────────────┐
│  表现层 Presentation                                               │
│  Tauri 2 WebView · React 19 · TypeScript · 设计令牌 · i18next      │
│  视图状态机 (Home / Browser / Settings) · 虚拟列表 · 任务中心      │
├────────────────────────────────────────────────────────────────────┤
│  应用层 Application (src-tauri, Rust)                              │
│  Tauri Commands/Events · JobManager · Settings · Recent/Favorites  │
│  ThemeService · LocaleService · Updater · IPC 单实例与参数转发     │
├────────────────────────────────────────────────────────────────────┤
│  核心库 omnizip-core (Rust crate, 无 UI 依赖)                      │
│  FormatRegistry · ArchiveReader/Writer · TreeIndex · 编码探测      │
│  SecurityPolicy · ExtractSink · 进度/取消 · 错误码目录             │
├─────────────┬─────────────┬─────────────┬──────────────────────────┤
│ 引擎 zip    │ 引擎 7z     │ 引擎 tar+   │ 引擎 rar(只读)·libarchive │
│ (zip crate) │(sevenz-rust2)│(tar/flate2/ │ (unrar FFI, 隔离 crate)  │
│             │             │ xz/zstd/lz4)│ (可选 feature)           │
├─────────────┴─────────────┴─────────────┴──────────────────────────┤
│  挂载层 omnizip-vfs (守护进程 omnizip-mountd 内运行)               │
│  ArchiveFs(VirtualFs) · 块缓存 · 寻址索引 · 固实块缓存             │
│  后端: WinFsp (Win) · FSKit / NFS 回环 (macOS) · FUSE3 (Linux)     │
├────────────────────────────────────────────────────────────────────┤
│  平台集成 Platform                                                 │
│  Win: COM Shell 扩展 DLL · 注册表 · MSIX 身份 · 任务栏进度        │
│  macOS: Services · Finder 扩展 · UTI 声明 · Dock                   │
│  Linux: .desktop/MIME · 文件管理器菜单 · D-Bus                     │
│  CLI: omnizip (clap) 静态链接核心库                                │
└────────────────────────────────────────────────────────────────────┘
```

依赖方向严格自上而下；核心库不依赖 Tauri，不输出任何本地化文案（只返回错误码与参数），保证 CLI 与 GUI 行为一致。

## 3. 进程模型

| 进程 | 职责 | 说明 |
|---|---|---|
| `omnizip-desktop` | 主窗口、应用层、核心库、作业线程池 | 单实例；二次启动将参数经 IPC 转发给已运行实例 |
| `omnizip`（CLI） | 命令行入口 | 静态链接核心库，不依赖 GUI 进程 |
| Windows `omnizip_shell.dll` | COM 进程内服务器（加载进 explorer.exe） | **不链接核心库**，只做：判断选中项、构造命令行、经命名管道唤起主程序 |
| macOS `OmniZipFinder.appex` | Finder 扩展（Swift） | 同理，只负责菜单与转发 |
| Linux 菜单文件 / nautilus-python | 声明式菜单 | 调用 CLI 或 GUI |
| `omnizip-mountd` | 挂载守护进程，托管虚拟文件系统后端与缓存 | 每用户一个，首次挂载时按需启动，空闲自动退出；GUI 退出后挂载可保持；见 [12-mount.md](12-mount.md) |
| （v2）`omnizip-worker` | 沙箱子进程 | 运行 C 引擎（unrar、libarchive），崩溃不影响 UI |

单实例协议：命名管道（Windows `\\.\pipe\omnizip-<sid>`）/ Unix domain socket（`$XDG_RUNTIME_DIR/omnizip.sock`），消息为 JSON 行：

```json
{"op":"extract","mode":"here","paths":["C:\\Users\\me\\a.zip"],"cwd":"C:\\Users\\me"}
```

## 4. 核心库 omnizip-core

### 4.1 关键抽象

```rust
pub trait ArchiveFormat: Send + Sync {
    fn id(&self) -> FormatId;                       // Zip, SevenZ, Tar, Rar, ...
    fn capabilities(&self) -> Capabilities;         // READ | WRITE | UPDATE | ENCRYPT | ENCRYPT_HEADERS
                                                    // | MULTI_VOLUME | SOLID | COMMENT | STREAMING
    fn sniff(&self, header: &[u8], ext: Option<&str>) -> Confidence;
    fn open(&self, src: Source, opts: OpenOptions) -> Result<Box<dyn ArchiveReader>>;
    fn create(&self, dst: Sink, opts: WriteOptions) -> Result<Box<dyn ArchiveWriter>>;
}

pub trait ArchiveReader: Send {
    fn index(&mut self, ctx: &JobCtx) -> Result<Arc<TreeIndex>>;   // 条目索引（懒加载、可取消）
    fn extract(&mut self, sel: &Selection, sink: &mut dyn ExtractSink,
               opts: &ExtractOptions, ctx: &JobCtx) -> Result<Report>;
    fn test(&mut self, ctx: &JobCtx) -> Result<Report>;
    fn open_entry(&mut self, id: EntryId, ctx: &JobCtx) -> Result<Box<dyn Read + Send>>; // 预览流
}

pub trait ArchiveWriter: Send {
    fn add(&mut self, src: EntrySource, ctx: &JobCtx) -> Result<()>;
    fn finish(self: Box<Self>, ctx: &JobCtx) -> Result<Report>;
}

pub struct Entry {
    pub id: EntryId,
    pub raw_name: Vec<u8>,          // 原始字节（用于重新解码）
    pub path: String,               // 解码后 UTF-8，已净化
    pub size: u64,
    pub packed_size: Option<u64>,
    pub mtime: Option<SystemTime>, pub atime: Option<SystemTime>, pub ctime: Option<SystemTime>,
    pub unix_mode: Option<u32>, pub win_attrs: Option<u32>,
    pub kind: EntryKind,            // File | Dir | Symlink{target} | Hardlink{target} | Other
    pub crc32: Option<u32>,
    pub encrypted: bool,
    pub method: Method,
    pub comment: Option<String>,
}

pub struct JobCtx {
    pub cancel: CancellationToken,
    pub progress: Box<dyn ProgressSink>,        // 节流后发往 UI / CLI
    pub passwords: Box<dyn PasswordProvider>,   // 回调：先试已知密码，再询问用户
    pub conflicts: Box<dyn ConflictResolver>,   // 回调：Ask / Skip / Overwrite / Rename / ...
    pub policy: SecurityPolicy,                 // 见 09
}
```

`ExtractSink` 的实现：
- `FsSink`：处理长路径（Windows `\\?\` 前缀）、保留名、时间戳、权限、符号链接策略、Mark-of-the-Web、新文件直接写入、替换已有文件时经临时文件 + rename、磁盘空间预检。
- `MemorySink`：预览与测试。
- `StreamSink`：CLI 的 stdout 管道。

### 4.2 格式探测

1. 读取头部 512 B 与尾部 64 KB。
2. magic 匹配：ZIP `50 4B 03 04` / `50 4B 05 06`、7z `37 7A BC AF 27 1C`、RAR `52 61 72 21 1A 07 00|01 00`、gzip `1F 8B`、xz `FD 37 7A 58 5A 00`、zstd `28 B5 2F FD`、bzip2 `42 5A 68`、tar 偏移 257 `ustar`、CAB `4D 53 43 46`、ISO 偏移 32769 `CD001`。
3. 自解压 EXE：扫描 PE 覆盖区（overlay）末尾的 ZIP 中央目录 / 7z 签名。
4. 复合扩展（`.tar.gz`、`.tgz`、`.tar.zst`）两层识别：外层单文件压缩 → 内层 tar。
5. 扩展名仅作兜底与歧义消解；magic 与扩展名冲突时以 magic 为准并在 UI 提示。

### 4.3 文件名编码修复（中文用户关键需求）

- ZIP 条目若未设置 UTF-8 标志（通用位 11）且无 Info-ZIP Unicode 扩展字段（0x7075），将**全部条目名字节拼接**后用 `chardetng` 探测；候选集限定为 GB18030、Big5、Shift_JIS、EUC-KR、CP437、CP866、CP1251、CP1252、ISO-8859-x。
- 置信度不足时按系统区域设置选默认（zh-CN → GB18030）。
- UI 提供"文件名编码"菜单，可手动切换并即时重解码整棵树；记忆每个压缩包的选择（按内容哈希）。
- RAR4 用 OEM 代码页；7z / RAR5 / tar(pax) 原生 Unicode，无需处理。
- 创建 ZIP 时始终写 UTF-8 标志；可选同时写 0x7075 扩展字段以兼容旧工具。

### 4.4 作业调度 JobManager

- 作业队列，并发度默认 `max(1, 物理核数 / 2)`，上限可配置；同一磁盘上的 I/O 密集作业默认串行。
- 每个 Job：`id`、`kind`、`state`（Queued / Running / Paused / Done / Failed / Cancelled）、`progress`（bytes_done/total、files_done/total、current_path）、`speed`、`eta`。
- 取消：`CancellationToken` 在每个条目边界与每 1 MB 数据块检查。
- 暂停：协作式检查点（v1.1）。
- 进度事件通过 Tauri `Channel` 推送，节流 ≤ 30 Hz，多条合并。
- 引擎内并行：7z LZMA2 / xz / zstd 的多线程由引擎参数控制；ZIP 多文件采用流水线（读取 → 压缩 → 写入）。

### 4.5 条目索引与前端分页

- `TreeIndex` 驻留 Rust：`HashMap<DirId, Vec<EntryId>>` + 排序缓存 + 过滤位图。
- 前端只请求可视窗口：`list_dir(archive, dir, offset, limit, sort, filter)` 返回 ≤ 500 行。
- 搜索在 Rust 端执行（子串 / glob / 正则），返回命中 ID 列表的分页。
- 百万级条目下 JS 端内存保持恒定。

### 4.6 配置与持久化

| 数据 | 格式 | 位置 |
|---|---|---|
| 设置 | TOML（带 `schema_version`，支持迁移） | `ProjectDirs::config_dir()/config.toml` |
| 最近 / 收藏 / 每压缩包编码记忆 | SQLite（rusqlite，bundled）。**M1 实现**：最近列表为 JSON（条目少、无查询需求）；收藏与编码记忆上线时迁移到 SQLite | `data_dir()/omnizip.db`（M1：`data_dir()/recent.json`） |
| 主题 | JSON | `config_dir()/themes/*.json` |
| 密码库（可选） | 系统钥匙串（`keyring` crate） | Credential Manager / Keychain / Secret Service |
| 日志 | `tracing` 滚动文件 | `cache_dir()/logs/` |

Shell 扩展与 CLI 读取同一份 `config.toml`（语言、菜单项配置），保证一致。

## 5. 应用层（src-tauri）

- Commands 按命名空间组织：`archive.*`、`job.*`、`settings.*`、`theme.*`、`locale.*`、`shell.*`、`preview.*`。
- Events：`job://progress`、`job://finished`、`archive://invalidated`、`settings://changed`、`theme://changed`。
- 使用的 Tauri 插件：`dialog`、`fs`（限制 scope）、`shell`（仅 `open`）、`os`、`window-state`、`single-instance`、`cli`、`updater`、`notification`、`store`、`deep-link`；窗口材质用 `window-vibrancy`。
- 安全：capabilities 最小化、禁止远程 URL、严格 CSP、前端不直接读文件系统，一切经 Command。

## 6. 前端架构

| 关注点 | 选择 |
|---|---|
| 框架 | React 19 + TypeScript + Vite |
| 状态 | Zustand（UI 状态）+ TanStack Query（Rust 查询缓存与失效）。**M1 实现**：仅 Zustand，Rust 查询结果由 store 直接持有，按 `archive://changed` 事件失效 |
| 组件基座 | Radix UI Primitives（无样式、可访问）+ 自研样式层。**M1 实现**：全部自研组件（对话框、分段控件、开关、菜单），可访问性属性手写 |
| 虚拟列表 | TanStack Virtual |
| 样式 | CSS Modules + CSS 自定义属性（主题令牌），禁止裸色值。**M1 实现**：两份全局样式表（`src/styles/tokens.css` 令牌、`app.css` 按组件分节），令牌规则不变 |
| 图标 | Lucide（界面）+ 自绘文件类型图标集（单色 / 双色两套）。**M1 实现**：内置 SVG 图标组件（`components/Icon.tsx`），未引入 Lucide |
| 动效 | CSS transitions 为主，复杂编排用 Motion（framer-motion），遵循 `prefers-reduced-motion`。**M1 实现**：仅 CSS transitions |
| i18n | i18next + i18next-icu |
| 窗口 | 主窗口；设置为主窗口内视图；预览为独立窗口；Shell 触发的操作用轻量进度窗 |

## 7. 关键决策记录（ADR 摘要）

| 编号 | 决策 | 被否选项 | 理由 |
|---|---|---|---|
| ADR-01 | Tauri 2 + Rust | Qt / Electron / Avalonia | 见 1.2 |
| ADR-02 | 纯 Rust 引擎优先，libarchive 为可选长尾引擎 | 全部走 libarchive | 跨平台链接成本、内存安全、可控的 feature 粒度 |
| ADR-03 | Shell 扩展不链接核心库 | 扩展内直接解压 | explorer.exe 稳定性、升级时文件锁、体积 |
| ADR-04 | 条目索引驻留 Rust，前端分页 | 整树序列化到 JS | 百万条目性能 |
| ADR-05 | 主题 = JSON 令牌 → CSS 变量 | 多套 CSS 文件 | 可自定义、可校验、可热更 |
| ADR-06 | 前端 i18next(JSON/ICU)，Rust 侧 Fluent，构建期一致性校验 | 单一 Fluent | 前端生态与工具链 |
| ADR-07 | 核心库只返回错误码 + 参数 | 核心库返回文案 | CLI / GUI / 日志各自本地化 |
| ADR-08 | 配置 TOML | JSON | 可读、可注释、Shell 扩展易解析 |
| ADR-09 | Windows 现代右键菜单走 IExplorerCommand + MSIX 包身份 | 仅经典菜单 | Win11 一级菜单可见性 |
| ADR-10 | 虚拟挂载运行在独立守护进程，后端 WinFsp / FSKit(NFS 保底) / FUSE3 | 在 GUI 进程内挂载 | 文件管理器对延迟敏感；挂载可脱离 GUI 存活；C 引擎崩溃隔离 |
| ADR-11 | 挂载的随机访问按格式分级（直读 / 句柄内流式 + 落盘缓存 / 固实块缓存 / 寻址索引） | 一律先解压到临时目录 | 大包秒开、磁盘占用可控 |

## 8. 性能预算

| 项目 | 预算 |
|---|---|
| 冷启动到首帧 | < 600 ms；到可交互 < 1 s |
| 列表首屏渲染 | < 100 ms（任何条目数） |
| 单次 IPC payload | < 1 MB |
| 空闲内存 | < 120 MB |
| 写入缓冲 | 256 KB–1 MB，Windows 使用 `FILE_FLAG_SEQUENTIAL_SCAN`，大文件预分配 |
| 进度事件频率 | ≤ 30 Hz |

## 9. 风险与缓解

| 风险 | 缓解 |
|---|---|
| Linux WebKitGTK 渲染差异 / 性能 | 在 Linux 降级模糊与半透明特效；CI 跑 Ubuntu 截图回归 |
| Win11 现代菜单需要包身份 | MSIX 为主渠道；EXE 安装器注册 Sparse Package 获得身份 |
| 7z 写入性能不及 7-Zip 原生 | sevenz-rust2 多线程 LZMA2；持续基准；必要时 FFI LZMA SDK |
| RAR 许可 | 仅解压；unrar 隔离为独立 crate 与（v2）独立进程 |
| 拖出到资源管理器 / Finder | Windows：自带 OLE drag source（`dragout.rs`，`DoDragDrop` + `SHCreateDataObject` 的 CF_HDROP），松开按钮时才把解压好的文件交出；不用 `tauri-plugin-drag`，它要求拖动开始时文件已就绪。macOS / Linux 待做 |
| Flatpak 沙箱限制 Shell 集成与挂载 | Flatpak 版本标注集成受限；推荐 .deb/.rpm/AppImage 获取完整体验 |
| macOS 挂载后端能力（FSKit 对虚拟文件系统的支持） | M0 实测后决定；NFS 回环为保底。WinFsp 许可问题已随 MIT 开源决定解决 |
