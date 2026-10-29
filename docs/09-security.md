# 09 · 安全设计

## 1. 威胁模型

| 要素 | 内容 |
|---|---|
| 保护对象 | 用户文件系统完整性、凭据（密码）、系统稳定性（explorer.exe / Finder）、隐私 |
| 攻击者 | 恶意压缩包（下载、邮件、即时通讯）、恶意文件名、本机其他用户（挂载可见性）、供应链（依赖、更新渠道） |
| 信任边界 | 压缩包内容 = **不可信**；网络 = 不可信；前端 WebView = 半可信（严格 CSP，不直接访问文件系统）；Rust 核心 = 可信；C 引擎 = 隔离对待 |

压缩软件的历史漏洞主要集中在：解析器内存错误、路径穿越（Zip Slip）、符号链接穿越、压缩炸弹、以及 Shell 扩展崩溃。以下逐项给出设计。

## 2. 解析不可信输入

- 纯 Rust 引擎优先（见 03）。
- C 引擎（unrar、libarchive、libzstd、liblzma）：版本锁定、`cargo-audit` 跟踪、v2 迁移到 `omnizip-worker` 子进程并施加平台沙箱：Windows AppContainer + Job Object（内存 / 句柄上限）；Linux seccomp-bpf + Landlock（或 bubblewrap）；macOS XPC 服务 + App Sandbox。
- 所有解析器的硬限制：条目数 ≤ 10,000,000、单个文件名 ≤ 32 KB、注释 ≤ 64 KB、目录深度 ≤ 256、头部声明的大小必须在文件范围内；整数运算使用 checked / saturating；索引构建可取消。
- `cargo-fuzz` 对每个解析器持续模糊测试；对 C 引擎用 AFL++；崩溃样本回归到 fixtures。

## 3. 路径与文件系统安全

每个条目路径经过固定管线，任何一步失败都记入报告：

1. **解码**：文件名编码修复（见 02 §4.3）。
2. **规范化分隔符**：`\` 与 `/` 统一；折叠重复分隔符。
3. **拒绝危险前缀**：绝对路径、驱动器前缀（`C:`）、UNC、`\\?\`、`..` 段（默认拒绝；可选"剥离"模式仅用于明确要求的 CLI 场景）。
4. **Windows 兼容**：保留名（`CON`、`PRN`、`AUX`、`NUL`、`COM1–9`、`LPT1–9`，含带扩展名形式）、尾随点 / 空格、`:`（ADS）、控制字符、`<>|"?*` 替换为 `_`。
5. **Unicode**：NFC 规范化；检测并提示 bidi 控制字符（如 U+202E 伪装扩展名）与零宽字符。
6. **大小写冲突**：目标文件系统不区分大小写时检测同目录冲突，按覆盖策略处理。
7. **最终校验**：目标路径 canonicalize 后必须以目标目录为前缀。
8. **防 TOCTOU**：Linux / macOS 用 `openat` + `O_NOFOLLOW` 逐级打开；Windows 用 `FILE_FLAG_OPEN_REPARSE_POINT` 检测重解析点；目标目录中途被换成符号链接时中止。

其他规则：
- **符号链接**：默认只创建目标在目标目录内的链接，且**全部延后到解压末尾创建**（防止"先建链接再经链接写入"）；硬链接同理；Windows 默认不创建符号链接（需特权），改写为普通文件。
- **权限**：剥离 setuid / setgid / sticky；可选不还原权限。还原的权限都再去掉进程 umask（与普通用户运行 `tar` 相同），umask 077 下解压出的内容别的用户读不到。文件先写入仅属主可读写（0600）的临时文件，就位后才放宽：有 Unix 权限且开启还原时用条目的权限（属主至少可读写），否则与新建文件相同（0666 去掉 umask；DOS 条目不采用 zip 库推算的 0664）。目录只还原本次解压新建的（不改动已存在的目录，包括目标目录本身），且在全部写完后才设置，只读目录也能写入内容；属主至少 `rwx`。新建的压缩包同样按 umask，原地更新或覆盖已有压缩包时保留其原权限。
- **长路径**：Windows 使用 `\\?\` 前缀与 `longPathAware`；总长 > 32,000 拒绝。
- **磁盘空间**：声明总大小与可用空间比较，不足时警告（允许继续，因为稀疏 / 炸弹情形声明值可能虚高）。
- **原子写入**：写入临时文件后 rename；中断时清理。

## 4. 压缩炸弹与资源耗尽

| 向量 | 对策 |
|---|---|
| 高膨胀比 | 单条目或总体膨胀比 > 1000:1 且输出 > 1 GB 时暂停询问（GUI）或拒绝（CLI，`--allow-high-ratio` 覆盖） |
| 嵌套炸弹 | 嵌套自动展开深度 ≤ 8；挂载不递归 |
| 重叠条目（overlap / quine） | ZIP 中央目录中 local header 偏移范围重叠 → 标记可疑并拒绝一次性全部解压 |
| 超大字典 | 7z 字典 > 1.5 GB 需确认；> 可用内存 70% 拒绝 |
| 条目数 / 声明大小 | 上限可配置；超过阈值 UI 显示警示横幅 |
| CPU 时间 | 所有作业可取消；UI 线程永不执行解码 |
| 挂载 | 见 12 §7（声明大小截断、超时、配额） |

## 5. 密码与凭据

- 内存中使用 `SecretString` + `zeroize`；不写日志、崩溃报告、IPC 日志；CLI 行内 `-p<pw>` 给出进程列表可见的警告。
- 可选保存到系统钥匙串（Credential Manager / Keychain / Secret Service），按用户命名或压缩包哈希索引；设置页可管理与清除。
- 尝试顺序：会话缓存 → 钥匙串（用户允许）→ 询问用户；无网络、无遥测。
- 写入仅支持 AES-256（ZIP AE-2、7z）；读取 ZipCrypto 时显示"弱加密"徽标并建议转换。
- 密码强度估计（zxcvbn 风格）仅在创建时显示。
- 守护进程中的密钥随卸载清零。

## 6. 来源标记传播

| 平台 | 机制 | 默认 |
|---|---|---|
| Windows | 读取压缩包的 `Zone.Identifier` ADS，ZoneId ≥ 3 时写入每个解压文件 | 开 |
| macOS | 传播 `com.apple.quarantine` 扩展属性 | 开 |
| Linux | 可选传播 `user.xdg.origin.url` | 关 |

与 SmartScreen / Gatekeeper 协同，可在"设置 → 安全"关闭。

## 7. 前端与 IPC

- 严格 CSP：`default-src 'self'`，脚本禁止 `unsafe-inline` / `unsafe-eval`，样式使用 nonce；禁止远程导航与外部资源。
- Tauri capabilities 最小化：按窗口授予命令；`fs` 插件 scope 仅限对话框返回的路径；`shell` 插件仅允许 `open` 且限制为 `file://` 目录与 `https://` 白名单。
- 前端不接触原始文件字节：图片经 Rust 重编码、文本经转义、文件名用 `textContent` 渲染。
- 命令参数在 Rust 侧校验：路径必须来自对话框 / 拖放句柄或用户显式输入后 canonicalize。
- 多窗口消息带来源校验；拖放数据只接受文件路径。

## 8. Shell 扩展与守护进程

- Windows DLL：无网络、无写文件、无 UI；`catch_unwind` 兜底；只读注册表与配置文件；与主程序同一证书签名。主程序启动时 `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32 | LOAD_LIBRARY_SEARCH_APPLICATION_DIR)` 防 DLL 搜索劫持。
- 命名管道 / Unix socket：ACL 限当前用户；消息上限 16 MB；JSON 严格解析；拒绝相对路径。
- `omnizip-mountd`：当前用户权限；不开启 `allow_other`；总内存上限；崩溃不影响主程序。
- macOS 扩展：通过 App Group 容器与 URL scheme 通信，参数白名单校验。

## 9. 供应链与发布

- `cargo-deny`（许可证、advisory、来源）、`cargo-audit`、`pnpm audit`、Renovate 自动更新依赖；锁文件提交。
- 代码签名：Windows Azure Trusted Signing（或 EV 证书）；macOS Developer ID + 公证 + stapling；Linux 包与 AppImage GPG 签名。
- 自动更新：Tauri updater，`latest.json` 与安装包均由 minisign 签名，私钥离线保存；校验版本单调递增防降级；HTTPS 强制。
- SBOM（CycloneDX）随每个发布；`SECURITY.md` 公布披露流程与 PGP 公钥；1.0 后评估漏洞赏金。
- CI：最小权限 token、受保护分支、必需审查、发布工作流需人工批准。
- 尽力可重现构建：固定工具链、`SOURCE_DATE_EPOCH`、构建日志公开。

## 10. 隐私

- 默认零遥测。
- 可选崩溃报告（自托管 Sentry），路径哈希化、文件名移除、密码永不包含。
- 更新检查仅发送版本、平台、架构；无账号系统。
- 最近文件列表可禁用 / 清除；缓存可一键清理；日志默认不含文件名。

## 11. 安全测试与流程

- 路径净化测试集：Windows 与 POSIX 各 200+ 用例，包括历史公开漏洞样本（Zip Slip 系列、符号链接穿越、重叠条目炸弹、UDF / HFS 解析崩溃样本）。
- 属性测试（proptest）：任意字节序列 → 创建 → 解压 → 一致。
- Fuzz：每解析器每周 ≥ 24 小时。
- 发布前安全清单：CSP、capabilities、签名、更新链、路径净化、炸弹限制、密码日志检查、Shell 扩展 AppVerifier。
- 1.0 前对核心库、Shell 扩展、挂载守护进程进行一次第三方审计（预算项）。
