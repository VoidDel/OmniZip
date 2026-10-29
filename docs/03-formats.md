# 03 · 压缩格式支持方案

## 1. 分级承诺

| Tier | 含义 | 质量承诺 |
|---|---|---|
| **Tier 1** | 读写完整支持，纯 Rust 引擎 | 完整测试矩阵、fuzz、性能基准、文档化参数 |
| **Tier 2** | 只读（浏览 / 解压 / 测试），纯 Rust 或隔离的 C 库 | 完整测试、fuzz；不承诺性能基准 |
| **Tier 3** | 可选 / best-effort，依赖 libarchive feature 或后续插件 | 冒烟测试；缺失依赖时在 UI 中灰显并说明 |
| 不支持 | 无可用合法实现 | 识别后给出明确提示（例如 ACE） |

## 2. Tier 1 格式（读写）

| 格式 | 扩展名 | 原地增删 | 加密 | 多卷 | 固实 | 注释 | 压缩方法 | 引擎 |
|---|---|---|---|---|---|---|---|---|
| ZIP | .zip .zipx .jar .war .apk .ipa .docx/.xlsx/.pptx .epub .xpi | ✓（复制原始数据，不重压） | 写 AES-256；读 AES / ZipCrypto | ✓（.z01…） | — | ✓ | 写：Store / Deflate / BZip2 / LZMA / Zstd；读另加 Deflate64 / XZ / PPMd；ZIP64 | `zip` |
| 7z | .7z | 全量重建 | AES-256 + 文件名加密 | ✓（.001…） | ✓ | — | LZMA / LZMA2 / PPMd / BZip2 / Deflate / Zstd；BCJ/BCJ2/ARM64 过滤器 | `sevenz-rust2` |
| TAR | .tar | 全量重建 | — | — | n/a | — | 无 | `tar` |
| TAR + 压缩 | .tar.gz .tgz / .tar.bz2 .tbz2 / .tar.xz .txz / .tar.zst .tzst / .tar.lz4 / .tar.br | 全量重建 | — | — | n/a | — | gzip / bzip2 / xz / zstd / lz4 / brotli | `tar` + 对应编解码 |
| 单文件压缩 | .gz .bz2 .xz .lzma .zst .lz4 .br | n/a | — | — | n/a | — | 同上 | 对应编解码 |

说明：
- ZIP 读 PPMd / Deflate64 仅解压；写入不提供，以保证最大兼容性。
- ZIP 与 gzip 写入默认多线程（`--threads`，0 = 全部核心）：ZIP 的小文件每个线程压缩一个、按原顺序写入，16 MiB 以上的 Deflate 文件与 gzip 流按 1 MiB 分块并行压缩，每块以前 32 KiB 为字典、以 sync flush 结束后拼接（与 pigz 相同，任何解压器都能读，压缩率差别在千分之几内）。加密的 ZIP 条目仍单线程写入。
- xz / zstd 写入默认多线程；zstd 提供 `--long` 与字典选项（高级）。
- Office / EPUB 等基于 ZIP 的文档在"打开"时默认交给系统关联程序，仅在用户显式"用 OmniZip 打开"时作为压缩包浏览。

## 3. Tier 2 格式（只读）

| 格式 | 扩展名 | 引擎 | 备注 |
|---|---|---|---|
| RAR 4 / RAR 5 | .rar .r00–.r99 .part1.rar | `unrar`（C++ 绑定，隔离 crate） | 多卷、加密（含头加密）、固实；恢复记录仅校验；**不提供创建**（许可限制） |
| CAB | .cab | libarchive | MSZIP / LZX / Quantum |
| ISO 9660 / UDF | .iso .img | libarchive | Joliet、Rock Ridge；UDF best-effort；可直接浏览并提取 |
| CPIO | .cpio（newc / odc / bin） | libarchive | initramfs 常见 |
| AR / DEB | .a .deb | 纯 Rust（`ar` 格式简单）+ 内层 tar | .deb 自动展开 `data.tar.*` / `control.tar.*` |
| RPM | .rpm | 纯 Rust 头解析 + 内层 cpio | payload 可能为 gz / xz / zstd |
| ARJ / LZH (LHA) | .arj .lzh .lha | libarchive | 老格式，日本/亚洲历史资料常见 |
| XAR / PKG | .xar .pkg | libarchive | macOS 安装包 |
| 自解压 SFX | .exe | 检测覆盖区 → 交给 ZIP / 7z / RAR 引擎 | 不执行任何代码 |
| 分割文件 | .001 .002…（无容器） | 内置拼接 | 识别 `.001` 为纯分割时提供"合并"操作 |

## 4. Tier 3 与明确不支持

| 格式 | 状态 | 说明 |
|---|---|---|
| WIM / ESD | Tier 3 | libarchive 支持部分 WIM；ESD（LZMS）不支持 |
| DMG | Tier 3 | 仅 zlib / bzip2 UDIF + HFS+ 分区浏览；APFS 不支持；macOS 上建议 `hdiutil` 挂载 |
| MSI / CHM / NSIS / VHD | Tier 3（v1.x） | 视需求评估 |
| ZPAQ / ALZ / EGG | Tier 3（v1.x） | ALZ/EGG 在韩国市场常见 |
| ACE | 不支持 | 无合法可用实现 |
| SFX 创建 | v1.x（Windows） | 需自带 7z SFX 模块，另行评估签名策略 |

## 5. 引擎选型与依赖策略

| crate / 库 | 用途 | 类型 | 许可 | 备注 |
|---|---|---|---|---|
| `zip` | ZIP 读写 | 纯 Rust | MIT | 启用 `aes-crypto` `deflate64` `bzip2` `zstd` `lzma` `xz` |
| `sevenz-rust2` + `lzma-rust2` | 7z 读写 | 纯 Rust | Apache-2.0 | 多线程 LZMA2、AES、BCJ 过滤器 |
| `tar` | TAR 读写 | 纯 Rust | MIT / Apache-2.0 | ustar / pax / GNU，长文件名，xattr |
| `flate2`（`zlib-rs` 后端） | Deflate / gzip | 纯 Rust | MIT / Apache-2.0 | 性能接近 zlib-ng |
| `liblzma`（或 `lzma-rust2`） | XZ / LZMA | C 绑定（可切纯 Rust） | 0BSD | 多线程编码 |
| `bzip2`（`libbz2-rs-sys` 后端） | BZip2 | 纯 Rust | MIT / Apache-2.0 | |
| `zstd` | Zstandard | C 绑定（libzstd） | BSD | 多线程、`--long`、字典 |
| `lz4_flex` | LZ4 frame | 纯 Rust | MIT | |
| `brotli` | Brotli | 纯 Rust | BSD / MIT | |
| `unrar` | RAR 解压 | C++ 绑定 | unrar 许可（仅解压） | 隔离为 `omnizip-engine-rar`，不参与任何压缩代码；v2 进程隔离 |
| libarchive | 长尾格式 | C 动态库 | BSD | 运行时 `dlopen`，缺失时相关格式灰显；Windows 随包附带 |
| `crc32fast` `sha2` `blake3` `md-5` | 校验与"哈希"面板 | 纯 Rust | | |
| `chardetng` + `encoding_rs` | 文件名编码探测 / 转换 | 纯 Rust | | |

依赖原则：
1. 任何在主进程中**解析结构**的 C 代码（unrar、libarchive）必须：有 fuzz 历史、版本锁定、v2 移入 worker 进程。
2. 只处理压缩流的 C 代码（libzstd、liblzma）风险较低，允许进程内使用，但保留切换纯 Rust 实现的 feature 开关。
3. 所有引擎通过 `FormatRegistry` 注册，CLI `omnizip formats` 可列出当前构建启用的格式与能力。

## 6. 统一参数模型

### 6.1 压缩参数 `WriteOptions`

```rust
pub struct WriteOptions {
    pub format: FormatId,
    pub level: Level,                 // Store | Fastest | Fast | Normal | Better | Best | Ultra
    pub method: Option<Method>,       // 不填则取格式默认
    pub threads: Threads,             // Auto | N
    pub solid: Option<SolidMode>,     // 7z：Off | PerExtension | Size(bytes) | All
    pub dictionary: Option<u64>,      // 高级
    pub word_size: Option<u32>,       // 高级
    pub encryption: Option<Encryption>, // { password: SecretString, encrypt_headers: bool }
    pub volume_size: Option<u64>,
    pub filters: Filters,             // include/exclude glob、跳过隐藏、符号链接策略
    pub path_mode: StorePath,         // Relative | RelativeToParent | NameOnly
    pub metadata: MetadataPolicy,     // 时间戳、权限、xattr（tar）、属性
    pub comment: Option<String>,
    pub post: PostActions,            // 删除源、打开结果、写后校验
}
```

级别到各格式原生参数的映射（UI 统一显示 7 档）：

| UI 档位 | ZIP Deflate | 7z LZMA2 (`mx`) | zstd | xz | bzip2 | lz4 | brotli |
|---|---|---|---|---|---|---|---|
| 存储 | 0 | 0 | — | — | — | — | — |
| 最快 | 1 | 1 | 1 | 0 | 1 | 1 | 1 |
| 快速 | 3 | 3 | 3 | 2 | 3 | 3 | 4 |
| 标准 | 6 | 5 | 6 | 6 | 6 | 6 | 6 |
| 较好 | 7 | 7 | 12 | 7 | 7 | 9 | 9 |
| 最好 | 9 | 9 | 19 | 9 | 9 | 12 | 11 |
| 极限 | 9 + zopfli（可选） | 9 + 大字典 + 固实 | 22（`--ultra --long`） | 9e | 9 | 12 | 11 + 大窗口 |

界面在滑块旁显示"预计耗时 / 预计体积"（基于首 64 MB 采样的快速估算，后台执行、可跳过）。

### 6.2 解压参数 `ExtractOptions`

```rust
pub struct ExtractOptions {
    pub destination: PathBuf,
    pub path_mode: ExtractPath,        // Full | Flat | SmartRoot
    pub overwrite: Overwrite,          // Ask | Skip | Overwrite | OverwriteOlder | Rename | KeepBoth
    pub keep_broken: bool,             // 保留 CRC 失败的文件（标记）
    pub metadata: MetadataPolicy,      // 还原时间戳、权限（默认去掉 setuid/setgid）
    pub symlinks: SymlinkPolicy,       // CreateIfInside | AsFile | Skip
    pub filename_encoding: Option<Encoding>,
    pub free_space_check: bool,
    pub post: PostActions,             // 打开目录、删除压缩包、关闭窗口
}
```

**SmartRoot（智能解压）规则**：若压缩包顶层只有一个目录且无其他文件 → 直接解压到目标；否则在目标下创建以压缩包名命名的文件夹。此规则是 Bandizip 式体验的核心，默认开启。

### 6.3 原地更新策略

| 格式 | 添加 / 删除 / 重命名 | 实现 |
|---|---|---|
| ZIP | ✓ | 未改动条目复制原始压缩数据（零重压），写入临时文件后原子替换；中央目录重建 |
| 7z / TAR.* | ✓（重建） | 全量重建，UI 显示耗时预估并要求确认 |
| RAR / 其他 Tier 2 | ✗ | 提供"转换为 ZIP / 7z 后编辑" |

## 7. 多卷

- 支持 ZIP spanned（`.zip` + `.z01…`）、7z（`.7z.001…`）、RAR（`.part1.rar` / `.r00`）。
- `VolumeSource` 把多个文件抽象为一个连续的 `Read + Seek`；缺卷时暂停并提示定位或跳过。
- 创建分卷预设：FAT32（4 GiB − 1 B）、DVD（4.37 GiB）、邮件（25 MB）、自定义。

## 8. 流式与管道

- 非 seek 源（stdin）：gz / bz2 / xz / zst / tar 原生流式；ZIP 走本地文件头顺序读取（降级模式，解压后再校验 CRC）；7z 需要 seek → CLI 落盘到临时文件后处理并提示。
- 输出到 stdout：`omnizip x a.tar.zst --stdout` 用于管道。
- 大文件：解压使用 `read → decode → write` 三阶段流水线，I/O 与解码重叠。

## 9. 预览与"用外部程序打开"

| 类型 | 方式 | 安全措施 |
|---|---|---|
| 文本 / 代码 | ≤ 2 MB 流入预览器（语法高亮） | 纯文本渲染，禁用 HTML |
| 图片 | Rust 侧 `image` 解码并重编码为 PNG 再传入 WebView | 不把原始可疑文件交给 WebView 解码器 |
| PDF | pdf.js（禁用脚本与外链） | 独立窗口、严格 CSP |
| 音视频 | 提取到缓存后交给系统播放器 | 不内嵌 |
| 嵌套压缩包 | 栈式进入子压缩包（面包屑显示层级） | 嵌套深度上限 8 |
| 任意文件 | 提取到 `cache_dir/open/<hash>/` 用系统程序打开 | 监视修改，修改后提示"更新到压缩包"（仅 UPDATE 能力格式） |

## 10. 挂载适配

所有可读格式都可挂载为虚拟卷（详见 [12-mount.md](12-mount.md)），但随机访问体验按格式内部结构分为四级，UI 在挂载前据此给出提示：

| 等级 | 格式 | 随机读 | 首次访问成本 |
|---|---|---|---|
| A 直读 | ZIP(Store)、ISO、TAR（未压缩）、CPIO、CAB | 原生 seek | 无 |
| B 条目内流式 | ZIP(Deflate / BZip2 / Zstd)、单文件 gz / xz / zst（作为单文件卷） | 前向快、后向需重解或落盘缓存 | 无；反复回退时自动落盘 |
| C 固实块 | 7z 固实、RAR 固实 | 需先解整个固实块 | 与块大小成正比，可预热 |
| D 单流需索引 | tar.gz / tar.bz2 / tar.xz（单块）/ tar.zst（无 seek 表） | 建索引后块级可寻址 | 首次挂载后台建索引并持久化 |

`FormatRegistry` 为每个格式声明 `MountClass`，并在 `Capabilities` 中加入 `MOUNT` 位；RAR 与 libarchive 引擎的挂载同样只读。

## 11. 测试资产

- `fixtures/<format>/`：正常、损坏（截断、CRC 错）、加密、多卷、ZIP64、超长路径（> 260）、乱码文件名（GBK / Shift_JIS / Big5 / CP866）、NFD 文件名、符号链接 / 硬链接、Zip Slip 样例、压缩炸弹（42.zip、嵌套、稀疏超大文件）。
- 差分测试：对同一 fixture 比较 OmniZip 与 7-Zip / libarchive / bsdtar 的条目清单与解压哈希。
- `cargo-fuzz`：每个解析器一个 target，CI 夜间运行；崩溃样本回归到 fixtures。
- 基准：`criterion` + 固定语料（Silesia、Linux 源码树、JPEG 集），输出到仪表板跟踪回归。
