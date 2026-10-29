# 07 · Shell 集成

## 1. 目标

用户在文件管理器里不打开 OmniZip 主窗口就能完成九成操作：右键解压 / 压缩 / 测试 / 挂载、双击打开、拖放到图标、设为默认程序。三平台操作语义一致，菜单文案随 OmniZip 语言设置本地化。命令行能力见 [08-cli.md](08-cli.md)，挂载见 [12-mount.md](12-mount.md)。

## 2. 菜单项规范（三平台统一）

菜单项由选区类型决定。判定只看扩展名（无扩展名文件最多读 8 个的 magic），保证菜单渲染 < 50 ms。

| 选区 | 菜单项 |
|---|---|
| 单个压缩包 | 用 OmniZip 打开 · 解压到此处 · 解压到 "名称\"（智能）· 解压到… · **挂载** · 测试 · 转换为… · 校验哈希 |
| 多个压缩包 | 分别解压到独立文件夹 · 全部解压到此处 · 全部挂载 · 全部测试 |
| 非压缩包文件 / 文件夹 | 添加到 "名称.zip" · 添加到 "名称.7z" · 添加到压缩包… · 压缩并分享（v1.x，邮件 / AirDrop / 附近共享） |
| 混合 | 上述两组并列 |
| 文件夹背景 | 在此新建压缩包…（可选） |

- **布局**：级联（默认，一个 "OmniZip" 顶级项展开子菜单）或平铺（最多 4 项置顶，其余入子菜单），用户可勾选显示哪些项、是否显示图标。
- **行为**："解压到此处" / "添加到 名称.zip" 直接以默认预设执行并显示轻量进度窗；带 "…" 的项（"解压到…"、"添加到压缩包…"、"转换为…"，见 04 §4.11）只弹出对应对话框的独立小窗，不打开主窗口，窗口高度随内容自适应；点"开始"后同一窗口转为进度窗，取消则直接关闭。
- **名称推导**：单文件去扩展名（`a.tar.gz` → `a`）；多选取公共父目录名；目标已存在时自动加序号。
- **智能解压**：顶层单目录则不再套一层文件夹（见 03 §6.2）。

所有平台的菜单文件 / 代码都从同一份 `platform/shell-menu.toml` + `locales/*/shell.ftl` 生成，避免三处手改不一致。

## 3. Windows

### 3.1 两套右键菜单，一套实现

| 菜单 | 注册方式 | 要求 |
|---|---|---|
| Windows 11 现代菜单（一级） | `IExplorerCommand` COM 服务器 + AppxManifest `desktop4:FileExplorerContextMenus` | 必须有包身份（MSIX 或 Sparse Package） |
| 经典菜单（Win10、Win11 "显示更多选项"、Shift+F10） | 同一 COM 类注册到 `HKCR\*\shellex\ContextMenuHandlers\OmniZip`、`HKCR\Directory\shellex\ContextMenuHandlers\OmniZip`、`HKCR\Directory\Background\shellex\…` | 无需身份 |

一个 Rust 编写的 COM DLL 同时服务两者；Win11 上两处都出现同一组菜单。Windows 会把包里登记的菜单同时列进经典菜单（"显示更多选项"），因此有包身份时不再注册经典菜单（否则经典菜单里出现两个 OmniZip），菜单在两处都是一个"OmniZip"子菜单；只有没有包、或菜单包注册失败时才注册经典菜单作为后备，菜单样式"平铺"只在这时生效（注册表里按样式注册对应的类），设置页在 Windows 上注明这一点。

### 3.2 COM 扩展 `omnizip_shell.dll`（Rust + `windows-rs`）

实现接口：`IExplorerCommand`、`IEnumExplorerCommand`（级联子菜单）、`IObjectWithSelection`、`IInitializeCommand`、`IClassFactory`；导出 `DllGetClassObject`、`DllCanUnloadNow`、`DllRegisterServer` / `DllUnregisterServer`。

| 方法 | 设计 |
|---|---|
| `GetState` | 遍历 `IShellItemArray`（上限 1000 项，超过按"混合"处理），仅看扩展名决定 `ECS_ENABLED` / `ECS_HIDDEN`；无扩展名文件最多读 8 个的前 16 字节 |
| `GetTitle` / `GetToolTip` | 从内嵌 FTL 取文案；语言 = `%APPDATA%\OmniZip\config.toml` → `GetUserDefaultUILanguage()`；按配置文件 mtime 缓存 |
| `GetIcon` | 返回 `"<dll 路径>,-<资源 id>"`，浅 / 深两套按 `AppsUseLightTheme` 选择。**当前实现**：一级菜单与"用 OmniZip 打开"用应用图标，其余各项是与应用同风格的线性图标（`icons/menu/<动作>-light.ico` / `-dark.ico`，`scripts/gen-menu-icons.mjs` 生成，16–48 px），随程序装在程序目录的 `menu-icons` 文件夹；子菜单与平铺样式相同，设置里关闭"显示图标"时都不显示 |
| `Invoke` | 构造 `omnizip-desktop.exe --shell <verb> --cwd <dir> --files-from <临时文件>`；若主程序已运行则通过命名管道转发；否则 `ShellExecuteEx` 启动；全程不读文件内容 |
| `EnumSubCommands` | 级联模式下返回子项枚举器 |

硬性约束：
- 最小依赖、`panic = "abort"` + 入口 `catch_unwind` 返回 `E_FAIL`、无网络、无 UI、无阻塞 I/O（配置文件读取有 50 ms 超时且异步刷新缓存）。**M1 实现**：DLL 链接 `omnizip-core`（仅扩展名表与魔数探测）与 `omnizip-i18n`（菜单文案），约 0.75 MB；每次 Explorer 创建菜单对象时同步读取配置，尚无 mtime 缓存与读取超时。
- COM 线程模型 `Both`，引用计数与 `DllCanUnloadNow` 正确实现，AppVerifier 与 `Application Verifier` 下无报错。
- 同时构建 **x64 与 ARM64** 两份 DLL；ARM 设备上的 Explorer 为 ARM64 进程，x64 DLL 不会被加载。
- 日志仅 ETW（可用 `wpr` 采集），默认静默。

### 3.3 包身份与安装

| 渠道 | 身份来源 | 说明 |
|---|---|---|
| MSIX（主渠道、winget、Store） | 原生 | manifest 声明 `windows.comServer`、`windows.fileTypeAssociation`、`desktop4:FileExplorerContextMenus`、`uap5:StartupTask`（可选托盘常驻） |
| EXE（NSIS）/ MSI（WiX） | 安装时注册 **Sparse MSIX Package**（`AllowExternalContent="true"`，指向安装目录） | 需与主程序相同证书签名；无身份时仅注册经典菜单 |
| Scoop / 便携版 | 无身份 | 仅经典菜单；设置页提供"注册 Win11 菜单"按钮（执行 Sparse Package 注册） |

升级与卸载：
- Explorer 可能持有旧 DLL 句柄：新版本安装到版本化目录（`shell\<ver>\omnizip_shell.dll`）并更新注册表指向；旧目录在下次启动时清理；必要时提示一键"重启资源管理器"。**M1 实现**（NSIS，`platform/windows/nsis/hooks.nsh`）：安装前把旧 DLL 改名为 `.old` 并标记重启删除，安装后运行 `omnizip-desktop.exe --register-shell`（不开窗口）：注册随安装包附带的菜单包 `omnizip_menu.msix` 并移除经典菜单的注册（菜单包注册失败时改为注册经典菜单），再把安装目录加入用户 PATH；卸载时 `--unregister-shell` 反向执行。菜单包是 Sparse Package（`platform/windows/msix/AppxManifest.sparse.xml`，名称 `OmniZip.ContextMenu`，`uap10:AllowExternalContent`），以安装目录为外部位置，只声明 Win11 一级菜单及其 COM 代理服务器，文件全部来自安装目录，不在开始菜单列出（`AppListEntry="none"`），程序本身照常以普通 EXE 运行、没有包身份——与 Bandizip 等 EXE 分发的软件做法相同。注册前先移除已注册的旧菜单包（例如另一安装目录的），因此升级与换目录都会重新指向当前安装。系统只注册受信任证书签名的包。正式证书到位前，菜单包用自签测试证书签名，程序在构建时内置该证书的公钥（`build.rs`，取自仓库中的 `platform/windows/msix/omnizip-test.cer`，用正式证书构建时为空；私钥不进仓库）：注册已签名的菜单包前若本机"受信任的人"（本地计算机）中没有它，就以管理员身份再启动自身（`--trust-cert`，弹出 UAC）导入，然后由用户自己的进程注册菜单包（注册按用户进行）。因此安装时自动导入，只需在 UAC 中确认一次；拒绝时安装照常完成，只有经典菜单，设置页显示"仅经典菜单"并可重试。导入的只是公钥证书，私钥不随程序分发；卸载时不移除证书。完整 MSIX（`platform/windows/msix/AppxManifest.xml`）保留给商店渠道，与菜单包不要同时安装，否则一级菜单出现两次。
- 卸载：注销 COM 与 ProgID、移除 Sparse Package、`SHChangeNotify(SHCNE_ASSOCCHANGED)`。

### 3.4 文件关联与默认程序

- 每种格式一个 ProgID：`OmniZip.zip`、`OmniZip.7z`、`OmniZip.rar`、`OmniZip.tar`…，含按格式区分的 `DefaultIcon`、`shell\open\command`、`shell\extract`（"解压到此处"动词）、`shell\mount`（"挂载"动词，仅可挂载格式）。**当前实现**（EXE）：`tauri.conf.json` 每个扩展名一条关联（ProgID `OmniZip.<扩展名>`，打开命令由打包器生成），安装钩子把各自的 `DefaultIcon` 指向 `$INSTDIR\filetypes\<扩展名>.ico` 并通知资源管理器刷新；"解压到此处""挂载"动词尚未加入 ProgID（右键菜单已提供）。
- 文件类型图标：白色文档页、右上折角，页面中轴一段拉链（呼应应用图标），其下居中一块格式色块印格式名（ZIP、7Z、RAR、TAR、TGZ…）。按家族配色：ZIP 系 `#2F6FED`、7Z `#7A5AF8`、RAR `#C0392B`、TAR 系 `#8A6D3B`、单文件压缩（gz、bz2、xz、zst、lz4、br）`#4B5B73`、光盘镜像 `#0B7F72`，与应用内格式徽章一致。每个 .ico 含 16–256 px，各尺寸都印格式名：16–24 px 按像素网格重画，色块加宽到接近整个图标，格式名用 3×5 像素字母（字体在这么小时会糊）；32 / 40 px 同样按像素网格绘制，格式名用字体并放大，另有拉链；48 px 起是 256 px 图按比例缩小。由 `scripts/gen-filetype-icons.mjs` 生成到 `icons/filetypes/`（文字用 Segoe UI Bold，需在 Windows 上运行）。完整 MSIX 的文件类型仍用应用图标。
- `HKLM\SOFTWARE\RegisteredApplications` + `Capabilities\FileAssociations`，以出现在"设置 → 默认应用"。
- Windows 10/11 不允许程序静默夺取默认：设置页"设为默认压缩软件"按钮打开 `ms-settings:defaultapps?registeredAppUser=OmniZip` 并给出引导。
- `OpenWithProgids` 让 OmniZip 出现在"打开方式"。
- Jump List：最近压缩包 + 任务（新建压缩包、打开）。
- 应用图标接受拖放：压缩包 → 打开；其他 → 新建压缩包。

### 3.5 其他 Windows 集成

- 任务栏进度（`ITaskbarList3::SetProgressValue/State`）。
- Mark-of-the-Web：读取压缩包的 `Zone.Identifier` ADS，按设置传播到解压文件（默认开启，与 SmartScreen 协同）。
- 可选托盘图标：显示任务与挂载，关闭窗口时后台保留任务。
- v2：`IPreviewHandler` 在预览窗格显示压缩包内容列表；`IThumbnailProvider` 显示包内首张图片。

## 4. macOS

### 4.1 入口

| 入口 | 机制 | 说明 |
|---|---|---|
| **Services**（必备） | Info.plist `NSServices` | 出现在右键菜单"服务"与"快速操作"；零额外进程；`NSSendFileTypes` 限定压缩包 UTI；文案 `ServicesMenu.strings` |
| **Finder Sync 扩展**（推荐） | `OmniZipFinder.appex`（Swift） | 在 Finder 右键菜单顶层直接显示 OmniZip 子菜单与工具栏按钮；监控目录 = 用户主目录 + 已挂载卷；用户需在"系统设置 → 通用 → 登录项与扩展 → Finder 扩展"启用，首启引导；与主程序通过 `omnizip://` URL scheme 或 UNIX socket 通信 |
| **App Intents**（v1.x） | Swift App Intents | "解压压缩包" / "压缩文件" / "挂载压缩包" 动作，可被"快捷指令"与 Spotlight 调用 |

### 4.2 文件关联（UTI）

- `CFBundleDocumentTypes` 声明：`public.zip-archive`、`org.7-zip.7-zip-archive`、`com.rarlab.rar-archive`、`public.tar-archive`、`org.gnu.gnu-zip-archive`、`org.gnu.gnu-zip-tar-archive`、`public.bzip2-archive`、`org.tukaani.xz-archive`、`public.iso-image`…；无公共 UTI 的格式（zst、lz4、cab、arj）通过 `UTImportedTypeDeclarations` 导入。
- 设为默认：`NSWorkspace.setDefaultApplication(at:toOpen:)`（macOS 12+，系统会弹确认）；设置页"将所有压缩格式设为用 OmniZip 打开"一键执行并逐类确认。
- 每格式独立文档图标（`.icns`）。

**当前实现**：`platform/macos/Info.plist`（由 `tauri.macos.conf.json` 合并进 `.app`）声明与 Windows 文件关联相同的 15 种格式（zip、zipx、7z、rar、tar、tgz、tbz2、txz、tzst、gz、bz2、xz、zst、lz4、br；rar 为 Viewer，其余 Editor），各带 `CFBundleTypeIconFile`（`icons/filetypes/<扩展名>.icns`，与 Windows 图标同一套绘制）。不用 Tauri 的 `fileAssociations` 生成，因为它不支持按类型设图标，macOS 上将其清空。系统未声明或较早版本未声明的类型以 `UTImportedTypeDeclarations` 导入：`com.winzip.zipx-archive`、`org.7-zip.7-zip-archive`、`com.rarlab.rar-archive`、`public.tar-bzip2-archive`、`org.tukaani.tar-xz-archive`、`org.facebook.tar-zstd-archive`、`org.tukaani.xz-archive`、`org.facebook.zstd-archive`、`org.lz4.lz4-archive`、`com.google.brotli-archive`；系统也声明时以系统的为准。Finder 只显示默认打开方式所声明的图标：zip、7z 等系统自带"归档实用工具"处理的格式仍显示系统图标，用户在"显示简介 → 打开方式 → 全部更改"选 OmniZip 后改用 OmniZip 的图标；rar、zst 等无其他处理程序的格式安装后即由 OmniZip 打开并显示其图标。Finder 打开文件（双击、打开方式、拖到 Dock 图标）以 `RunEvent::Opened` 送达而非命令行参数：主窗口开始监听前到达的请求暂存，由前端注册监听后调用 `app_take_launch` 取走，之后直接发 `app://launch`；一次打开多个文件时只打开第一个（与 Windows 的命令行启动一致）。已在 macOS 上用调试构建实机验证：rar / zst 显示 OmniZip 图标，冷启动与运行中打开都显示该压缩包。未做：设为默认（`NSWorkspace.setDefaultApplication`）、`InfoPlist.strings` 本地化类型名称、`public.iso-image`。

### 4.3 行为

- Dock 图标拖放、Dock 进度徽标、菜单栏常驻图标（可选）。
- 完成后在 Finder 中定位结果：`NSWorkspace.activateFileViewerSelecting`。
- 隔离属性 `com.apple.quarantine` 传播到解压文件（默认开，与 Gatekeeper 协同；对应 Windows MOTW）。
- 直接分发版：公证 + 硬化运行时，不启用 App Sandbox（需任意路径读写）。App Store 版需 Sandbox + security-scoped bookmarks，后续评估。
- 挂载卷出现在 `/Volumes/<名称>` 与 Finder 侧栏，可"推出"。

## 5. Linux

### 5.1 文件关联

- `omnizip.desktop`：`MimeType=application/zip;application/x-7z-compressed;application/vnd.rar;application/x-tar;application/gzip;application/x-bzip2;application/x-xz;application/zstd;application/x-lz4;application/x-cpio;application/x-iso9660-image;…`，`Exec=omnizip-desktop %U`，`Actions=NewArchive;ExtractHere;Mount;`。
- `omnizip-mime.xml` 补充 shared-mime-info 中缺失的类型（如 `.tar.zst`、`.tzst`）。
- 安装后执行 `update-desktop-database`、`update-mime-database`；设置页"设为默认"执行 `xdg-mime default omnizip.desktop <types>`。
- 图标：hicolor 多尺寸 + `-symbolic` 版本。

### 5.2 文件管理器右键菜单

| 文件管理器 | 机制 | 位置 |
|---|---|---|
| Dolphin（KDE） | ServiceMenu `.desktop`（`X-KDE-Submenu=OmniZip`，`MimeType=` 过滤，`Name[zh_CN]=`） | `/usr/share/kio/servicemenus/omnizip.desktop` 或 `~/.local/share/kio/servicemenus/` |
| Nautilus（GNOME Files 43+） | nautilus-python 扩展（`MenuProvider`，gettext） | `/usr/share/nautilus-python/extensions/omnizip.py`；无 nautilus-python 时回退 Nautilus Scripts（`~/.local/share/nautilus/scripts/`） |
| Nemo（Cinnamon） | `.nemo_action` | `/usr/share/nemo/actions/` |
| Caja（MATE） | caja-python 或 scripts | 同 Nautilus |
| Thunar（XFCE） | Custom Actions | 安装脚本合并进 `~/.config/Thunar/uca.xml` |
| PCManFM(-Qt) | `.desktop` 文件管理器 actions | `~/.local/share/file-manager/actions/` |

所有菜单项最终调用 `omnizip-desktop --shell <verb> …`，由构建脚本从 `shell-menu.toml` 生成。

**当前实现**（Nautilus）：`platform/linux/nautilus/omnizip.py` 是 nautilus-python 扩展，只做一层外壳：右键时把所选路径交给 `omnizip shell-menu`（命令行的隐藏子命令，读标准输入、输出 JSON），由它按 Windows 菜单同一套规则（`omnizip_shell::menu`：哪些选择显示哪些项、按设置的界面语言生成标题、子菜单或平铺、设置里关掉的项）给出菜单项，设置改动无需重新注册；选中某项时把选择写入临时列表文件，启动 `omnizip-desktop --shell <verb> --cwd <目录> --files-from <列表>`。设置页"添加到右键菜单"或 `omnizip-desktop --register-shell` 把它装到 `$XDG_DATA_HOME/nautilus-python/extensions/`（填入本机两个程序的路径），需要 python3-nautilus，`nautilus -q` 后生效。"解压到‘名称’"中的路径分隔符由代码按平台补上（Linux 为 `/`）。其他文件管理器尚未接入。

### 5.3 打包形态对集成的影响

| 形态 | 文件关联 | 右键菜单 | 挂载 |
|---|---|---|---|
| .deb / .rpm / AUR | 完整 | 完整（系统级安装） | 完整 |
| AppImage | 首次运行"集成到系统"后完整 | 用户级安装 | 完整 |
| Flatpak | 完整（exports） | 需用户一键复制菜单文件到 `~/.local/share/…`；需 `--filesystem=host` | 不可用（沙箱内无 FUSE 挂载权限），设置页说明 |

### 5.4 其他

- D-Bus 服务名 `org.omnizip.Desktop`：单实例、`org.freedesktop.Application` 激活协议。
- 完成后定位文件：`org.freedesktop.FileManager1.ShowItems`。
- 通知：`org.freedesktop.Notifications`，带"打开文件夹"动作。

## 6. 参数协议与单实例

GUI 可执行文件接受：

```
omnizip-desktop [--open <archive>]
                [--shell open|extract-here|extract-smart|extract-to|add-zip|add-7z|add|test|mount|convert]
                [--cwd <dir>] [--files-from <list-file>] [<paths>…]
```

- 第二个实例通过 `tauri-plugin-single-instance` 把完整参数与工作目录转发给首个实例后退出。
- Shell 触发的操作只显示轻量进度窗（见 04 §4.4）或独立对话框窗口（`extract-to`、`add`、`convert`）；需要交互（密码、冲突、错误）时在该窗口内扩展，不弹主窗。
- `--files-from` 用于规避命令行长度上限（Windows 32 K 字符）。

## 7. 设置页"右键菜单与集成"

- 平台检测与状态：已注册 / 未注册 / 需要重启资源管理器 / 需要启用 Finder 扩展 / 缺少 nautilus-python。
- 菜单样式（级联 / 平铺）、显示项勾选（按菜单顺序）、图标开关（Linux 不显示：Nautilus 菜单没有图标）、实时预览：按"一个压缩包 / 多个压缩包 / 文件和文件夹"分别显示实际会出现的项（与 `applies` 相同），图标与菜单里的一致。
- "设为默认压缩软件"按钮 + 系统设置引导。
- CLI：加入 PATH、安装 Shell 补全（见 08）。
- "修复集成"一键重新注册。

## 8. 验收矩阵

| 平台 | 验证点 |
|---|---|
| Windows 10 22H2、Windows 11 23H2 / 24H2+（x64 与 ARM64） | 现代与经典菜单均出现；文案随语言；多选 500 项菜单延迟 < 50 ms；AppVerifier 无错误；升级后 Explorer 不需重启即可用新 DLL（或正确提示） |
| macOS 12 – 26（Intel 与 Apple Silicon） | Services 可见；Finder 扩展可启用并显示；默认应用切换生效；公证通过 |
| Ubuntu 22.04 / 24.04 GNOME、Fedora KDE、Linux Mint Cinnamon、Xubuntu | 菜单出现并执行；MIME 默认生效；AppImage 集成流程可用 |
| 全部 | 卸载后注册表 / plist / 菜单文件无残留 |
