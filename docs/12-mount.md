# 12 · 虚拟挂载：把任意压缩包当作磁盘使用

## 1. 目标

像 Windows 双击 ISO 得到一个盘符、macOS 双击 DMG 得到一个卷那样，**OmniZip 能读取的任何压缩包都可以一键挂载为虚拟卷**，内容按需解压、不落盘整包：

| 平台 | 挂载形态 | 文件管理器表现 |
|---|---|---|
| Windows | 盘符（默认从 Z: 向前取空闲）或 NTFS 目录挂载点 | 资源管理器"此电脑"与侧栏显示，可"弹出" |
| macOS | `/Volumes/<名称>` | Finder 侧栏与桌面显示，可"推出" |
| Linux | `/run/media/$USER/<名称>`（有 udisks 权限）或 `$XDG_RUNTIME_DIR/omnizip/mnt/<名称>` | 文件管理器侧栏显示（`x-gvfs-show`），可"卸载" |

典型用途：直接从 50 GB 的包里播放一个视频、用 IDE 打开包内源码、挑几张图、比对两个版本，而不必先解压。

## 2. 用户体验

- **入口**：命令栏"挂载"按钮；右键菜单"挂载"；CLI `omnizip mount`；可把 ISO / IMG 的双击默认动作设为"挂载"（与系统行为一致，需用户确认）。
- **一键默认**：不弹选项，挂载后自动在文件管理器打开；卷名 = 压缩包名去扩展名（非法字符替换）。
- **高级选项**（弹出面板）：挂载点 / 盘符、只读 / 可写（仅 ZIP，v1.x）、卷标、退出应用后是否保持、缓存上限、"离线属性"（避免资源管理器自动读取内容生成缩略图）。
- **管理**：侧栏"已挂载"分组列出全部挂载（名称、挂载点、缓存占用、打开、卸载）；状态栏挂载徽标；托盘 / 菜单栏图标菜单"卸载全部"；系统原生的"弹出 / 推出 / 卸载"同样生效并同步回 UI。
- **加密包**：挂载时询问密码；密钥仅驻留守护进程内存，卸载即清零。
- **源文件变更**：压缩包被修改 / 删除 / 移动 → 自动卸载并通知。Windows 上对源文件加共享读锁阻止删除；macOS / Linux 用 inode 监视。
- **固实包提示**：首次访问需解压整个固实块。挂载前显示预计时间，提供"后台预热"（按块顺序预解到缓存）。
- **退出行为**：默认退出应用时卸载全部；可选"保持挂载"（守护进程常驻，托盘保留入口）。
- **恢复**：登录后可选自动重新挂载上次的卷。

## 3. 平台后端

| 平台 | 主后端 | 备选 | 说明 |
|---|---|---|---|
| Windows | **WinFsp**（`winfsp` crate） | ProjFS（系统内置"投影文件系统"，无驱动，但只能投影到 NTFS 目录且会在磁盘留占位文件）；Dokany | WinFsp 性能与兼容性最佳（rclone、Cryptomator、SSHFS-Win 同款），支持盘符与目录挂载点、异步 I/O。需安装一次签名驱动：安装器提供可选组件，或首次使用时按需下载并校验签名与哈希。许可：GPLv3 + FLOSS 例外；OmniZip 采用 MIT 开源，符合该例外，可直接使用 |
| macOS | **FSKit** 文件系统扩展（macOS 15+） | 进程内 NFSv3 服务 + `mount_nfs localhost`（`nfsserve` crate，零安装）；macFUSE（若用户已安装） | FSKit 为 Apple 官方用户态文件系统框架，无 kext，需用户在"系统设置 → 文件系统扩展"启用。对非块设备（虚拟）资源的支持程度需在 M0 验证；NFS 回环方案无需安装、macOS 12+ 全可用，作为保底 |
| Linux | **FUSE3**（`fuser` crate，直接对接 `/dev/fuse`） | — | 依赖 `fusermount3`（各发行版标配）；Flatpak 沙箱内不可用，设置页提示改用 .deb / .rpm / AppImage |

统一接口：

```rust
pub trait MountBackend: Send + Sync {
    fn capabilities(&self) -> BackendCaps;   // DRIVE_LETTER | DIR_MOUNTPOINT | WRITABLE | SYMLINKS | CASE_SENSITIVE | XATTR
    fn health(&self) -> BackendHealth;        // Ready | NotInstalled | NeedsEnable | VersionMismatch
    fn mount(&self, fs: Arc<dyn VirtualFs>, opts: &MountOptions) -> Result<MountHandle>;
    fn unmount(&self, h: MountHandle, force: bool) -> Result<()>;
}

pub trait VirtualFs: Send + Sync {
    fn lookup(&self, parent: NodeId, name: &OsStr) -> Result<Attr>;
    fn getattr(&self, node: NodeId) -> Result<Attr>;
    fn readdir(&self, dir: NodeId, cursor: u64, max: usize) -> Result<Vec<DirEntry>>;
    fn open(&self, node: NodeId, flags: OpenFlags) -> Result<FileHandle>;
    fn read(&self, fh: FileHandle, offset: u64, size: u32) -> Result<Bytes>;
    fn release(&self, fh: FileHandle);
    fn readlink(&self, node: NodeId) -> Result<PathBuf>;
    fn statfs(&self) -> Result<FsStats>;
    // 可写（ZIP overlay，v1.x）：create / write / unlink / rename / setattr / fsync
}
```

`ArchiveFs` 实现 `VirtualFs`，复用 `omnizip-core` 的 `TreeIndex`（目录结构）与 `open_entry`（数据流）。接口只接受 `NodeId`，不接受路径字符串，天然无路径逃逸。

## 4. 进程模型：`omnizip-mountd`

- 每用户一个守护进程，托管全部挂载；GUI / CLI 通过单实例同一 IPC 通道发起 `mount` / `unmount` / `list` / `warm`。
- 理由：资源管理器与 Finder 对文件系统响应延迟敏感，UI 进程的渲染与 GC 不应影响；挂载可在 GUI 退出后保持；CLI 无 GUI 也能用；C 引擎（unrar、libarchive）崩溃不波及 UI。
- 生命周期：首次挂载时按需启动；无挂载且空闲 5 分钟自动退出；系统登出时内核清理挂载，守护进程把"上次挂载列表"写入数据库供恢复。
- 线程：每个挂载一个 I/O 线程池（默认 4 线程），解压在工作线程，文件系统回调快速返回（WinFsp 使用异步响应）。
- 守护进程崩溃：内核向应用返回 EIO，GUI 检测到后提示"重新挂载"。

## 5. 按格式的随机访问策略

挂载体验的关键是**随机读**。不同格式差异很大，需要分别处理：

| 格式 | 随机读天然能力 | 策略 |
|---|---|---|
| ZIP（Store）、ISO、TAR（未压缩）、CPIO、CAB（MSZIP 块独立） | 直接 seek | 零拷贝直读，体验最佳 |
| ZIP（Deflate / BZip2 / Zstd，每条目独立压缩） | 条目内只能顺序 | 每个打开句柄维护解码器位置；前向 seek 丢弃数据；后向 seek ≤ 64 KB 用环形缓冲；更远则重开流。检测到反复后向 seek（视频拖动、随机访问数据库文件）→ 后台把该条目完整解压到磁盘缓存并切换为直读 |
| 7z / RAR 固实块 | 必须从块首解到目标条目 | 首次访问某固实块：整块解压到磁盘缓存（< 256 MB 时放内存），块内所有条目随后 O(1)；挂载前提示；可"预热全部" |
| tar.gz | 单一流，无索引 | 首次挂载后台构建 zran 式检查点索引（每 4 MB 保存 32 KB 窗口），持久化到 `cache_dir/mount-index/<blake3(path,size,mtime)>.idx`；索引就绪前随机读走"重解 + 缓存" |
| tar.xz（多块流） | 块级可寻址 | 直接利用 xz 块索引（`xz -T` 产物天然多块）；单块流退化为 tar.gz 策略 |
| tar.zst | 帧级可寻址 | 若带 seekable 跳过帧表则直接利用；否则同 tar.gz 策略 |
| tar.bz2 | 块级（900 KB） | 扫描块边界建索引 |
| 嵌套压缩包 | — | 卷内呈现为普通文件，不递归挂载；用户可对其再次"挂载" |

缓存：
- 内存块缓存：LRU，默认上限 256 MB（可配置）。
- 磁盘缓存：默认 2 GB 或剩余空间 10%（取小），位置可配置；卸载时清理，可选保留以加速再次挂载。
- 索引缓存：长期保留，按源文件（路径、大小、mtime）哈希命中。

## 6. 文件系统语义

| 方面 | 设计 |
|---|---|
| 名称 | 与解压相同的净化规则；Windows / macOS 默认大小写不敏感，同目录大小写冲突追加 ` (2)`；非法字符替换为 `_` 并在"属性"中显示原名；超长名截断但保留扩展名；文件名编码修复（见 02 §4.3）同样生效 |
| 元数据 | mtime / atime / ctime；Unix mode（Linux / macOS，默认去掉 setuid / setgid）；Windows 只读属性，可选 OFFLINE 属性；大小 = 声明的未压缩大小 |
| 符号链接 | Linux / macOS：目标在卷内时呈现为符号链接，否则呈现为内容为目标路径的普通文件；Windows：一律普通文件 |
| 硬链接 | 呈现为独立文件 |
| 卷信息 | 卷标 = 压缩包名；文件系统名 `OmniZip`；序列号 = 压缩包哈希前 32 位；Windows 上以**网络驱动器**类型呈现（系统对卷内可执行文件按非本地信任处理，且搜索索引器不索引） |
| 可写（v1.x，仅 ZIP） | 写时复制 overlay 目录；新建 / 修改 / 删除记入日志；卸载时询问"保存到压缩包 / 另存为 / 放弃"，保存走 ZIP 原地更新（见 03 §6.3）；期间源包加锁 |

## 7. 安全

- **不执行任何内容**，但用户可能在卷内双击可执行文件：首次挂载弹出一次性风险提示；Windows 网络驱动器类型触发 SmartScreen 区域检查；macOS 卷内文件附加 `com.apple.quarantine`。
- **压缩炸弹**：只暴露声明大小；读取时实际输出超过声明大小即截断并返回 EIO；单条目膨胀比上限与解压策略相同；缓存配额限制磁盘占用。
- **超时保护**：任何文件系统回调 > 30 s 返回 EIO 而不是阻塞，避免资源管理器假死；预热作业可取消。
- **路径逃逸**：接口只用 `NodeId`，不存在 `..` 问题。
- **权限**：守护进程以当前用户身份运行；不开启 `allow_other`，其他用户看不到挂载；WinFsp 驱动安装需要一次管理员权限，安装包签名校验。
- **资源上限**：每卷打开句柄 ≤ 4096；每卷内存上限；守护进程总内存上限（默认 1 GB，超出拒绝新挂载）。
- **密码**：`SecretString` + `zeroize`，不写日志、不落盘。

## 8. CLI

```
omnizip mount   <archive> [--at <盘符:|路径>] [--label <名称>] [--rw]
                          [--password-file <F>] [--cache <大小>] [--warm] [--detach]
omnizip unmount <archive | 挂载点 | --all> [--force]
omnizip mounts  [--json]
omnizip mount   <archive> --exec -- <命令…>        # 临时挂载，命令结束后自动卸载
```

退出码：`0` 成功；`20` 后端未安装或未启用；`21` 挂载点被占用；`22` 需要密码；`23` 源文件不可读；`24` 守护进程不可达。

## 9. 设置页"挂载"

- 后端状态与引导：WinFsp 未安装 → 下载安装按钮；FSKit 扩展未启用 → 打开系统设置；`fusermount3` 缺失 → 安装命令提示。
- 默认挂载位置策略（盘符范围 / 目录）。
- 缓存上限与位置、索引缓存清理。
- 退出时自动卸载、启动时恢复挂载、固实包预热策略、OFFLINE 属性开关。
- 已挂载列表（与侧栏同源）。

## 10. 性能目标

| 指标 | 目标 |
|---|---|
| 挂载完成（10 万条目 ZIP） | < 1 s |
| 目录列举 1 万项 | < 50 ms |
| 顺序读吞吐（Store / Deflate） | ≥ 解压吞吐的 80% |
| 缓存命中读延迟 | < 1 ms |
| 视频拖动（Deflate ZIP 内 2 GB MP4） | 首次回退 ≤ 2 s 切换落盘，后续即时 |
| 固实 7z 1 GB 块首次访问 | 与解压该块耗时相同，UI 有进度 |

## 11. 里程碑

| 阶段 | 内容 |
|---|---|
| M0 | 三平台 PoC：WinFsp 盘符、FSKit 与 NFS 回环验证、`fuser`；ZIP Store / Deflate 只读 |
| M2 | 全部 Tier 1 / 2 格式只读；固实块缓存；tar.gz 索引；守护进程；UI / CLI / 右键入口 |
| M3 | ZIP 可写 overlay；预热；索引持久化；恢复挂载 |
| v2 | ProjFS 备选后端；与系统搜索（Windows Search / Spotlight）的排除或集成策略 |

## 12. 开放问题与决策

| 事项 | 状态 |
|---|---|
| WinFsp 许可与分发 | **已解决**（2026-10-27）：项目采用 MIT 许可，符合 WinFsp 的 FLOSS 例外，可随包分发或按需下载，无需商业许可 |
| macOS 后端选择 | **M0 实测后决定**。实测清单：(a) FSKit 能否以非块设备资源挂载，并在 Finder 侧栏显示与推出；(b) 1 万项目录列举与随机读延迟；(c) 用户启用扩展的摩擦；(d) 最低支持的 macOS 版本；(e) NFS 回环在非 root 下的挂载权限。任一关键项不满足则以 NFS 回环为主后端、FSKit 延后 |
| 覆盖系统的 ISO 双击行为 | 待定，默认关闭 |
| Spotlight / Windows 搜索索引 | 默认排除挂载卷，待实测 |

## 13. 实施状态（2026-10-27）

**已完成：挂载核心 `crates/omnizip-vfs`**（平台无关，不依赖任何驱动）。

| 部分 | 内容 |
|---|---|
| `VirtualFs` | 按 §3 的接口实现，只接受 `NodeId`；附 `resolve`（供 WinFsp 这类按路径回调的后端使用，拒绝 `..`）与 `read_all` |
| `ArchiveFs` | 基于 `TreeIndex` 呈现目录；首次读取某条目时解码进缓存，之后按偏移随机读；固实包（C 类）首次访问一次性预热全部条目；`warm()` 手动预热 |
| 名称（§6） | Windows 非法字符与设备名替换、尾部点与空格处理、超长名保留扩展名截断、同目录大小写冲突追加 ` (2)` |
| 链接（§6） | 卷内目标呈现为符号链接，卷外目标呈现为内容为目标路径的普通文件；Windows 一律普通文件；硬链接呈现为独立文件 |
| 缓存（§5） | 小条目放内存、大条目写入私有临时目录；内存与磁盘两个池各自限额、LRU 淘汰；被淘汰但仍被读取的内容保持有效，最后一个持有者释放后删除临时文件 |
| 安全（§7） | 解码输出超过声明大小即返回 EIO；只读卷（不可写、去掉 setuid / setgid）；每卷句柄上限；加密条目无密码时返回拒绝访问 |
| 后端探测（§9） | `probe()` 报告平台主后端的系统侧状态（WinFsp 注册表与 DLL、`/dev/fuse` 与 `fusermount3`、Flatpak）以及本构建是否包含后端；桌面应用设置页"挂载"显示实时状态 |

测试：19 项（真实 ZIP、加密 ZIP、固实 7z、手工构造含链接与冲突名的 TAR，以及模拟读取器的炸弹、并发与规模场景）；模糊测试目标 `fuzz/fuzz_targets/mount.rs`。规模参考（release 构建、模拟读取器，不含 ZIP 解析）：10 万条目建卷约 31 ms，1 万项目录列举约 4 ms。

**未开始**：`omnizip-mount-winfsp`（本机未安装 WinFsp，安装驱动需要管理员权限）、`omnizip-mount-fuse`、macOS 后端（FSKit 实测待 M0 完成）、`omnizip-mountd` 守护进程、CLI `mount` / `unmount` / `mounts`、tar.gz 检查点索引与 ZIP 条目内的流式回退（当前一律整条目解码进缓存）。界面中的"挂载"对话框仍显示"暂不可用"。
