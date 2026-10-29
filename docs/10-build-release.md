# 10 · 工程、构建与发布

## 1. 仓库结构

```
OmniZip/
├─ Cargo.toml                       # Rust workspace
├─ rust-toolchain.toml              # 固定 stable 版本
├─ package.json  pnpm-workspace.yaml
├─ crates/
│  ├─ omnizip-core/                 # 抽象、格式注册表、TreeIndex、编码修复、安全策略、作业
│  ├─ omnizip-engine-zip/
│  ├─ omnizip-engine-7z/
│  ├─ omnizip-engine-tar/           # tar + gz / bz2 / xz / zst / lz4 / br
│  ├─ omnizip-engine-rar/           # unrar FFI（隔离，可通过 feature 关闭）
│  ├─ omnizip-engine-libarchive/    # 可选 FFI，运行时 dlopen
│  ├─ omnizip-vfs/                  # 挂载：VirtualFs、ArchiveFs、块缓存、寻址索引
│  ├─ omnizip-mount-winfsp/         # Windows 后端
│  ├─ omnizip-mount-fuse/           # Linux 后端
│  ├─ omnizip-mount-nfs/            # macOS 保底后端（NFS 回环）
│  ├─ omnizip-ipc/                  # 单实例协议、守护进程客户端
│  ├─ omnizip-i18n/                 # Fluent 加载、错误码渲染
│  ├─ omnizip-cli/                  # bin: omnizip
│  ├─ omnizip-mountd/               # bin: omnizip-mountd
│  └─ omnizip-shell-win/            # cdylib: omnizip_shell.dll
├─ apps/
│  └─ desktop/
│     ├─ src-tauri/                 # Tauri 应用（bin: omnizip-desktop）
│     │  ├─ src/{commands,services,ipc,platform}/
│     │  ├─ capabilities/
│     │  └─ tauri.conf.json
│     └─ src/                       # React 前端
│        ├─ app/                    # 视图状态机、窗口引导
│        ├─ features/{home,browser,compress,extract,mount,tasks,preview,settings}/
│        ├─ components/             # 设计系统组件
│        ├─ design/                 # tokens.css、主题桥接
│        ├─ i18n/
│        └─ lib/                    # Tauri 调用封装、query hooks
├─ platform/
│  ├─ shell-menu.toml               # 右键菜单定义（单一真相）
│  ├─ windows/{AppxManifest.xml,sparse/,wix/,nsis/,icons/,registry/}
│  ├─ macos/{Info.plist.in,entitlements/,FinderSync/,FSKitExtension/,Services/,dmg/}
│  └─ linux/{omnizip.desktop,mime/,servicemenus/,nautilus/,nemo/,thunar/,flatpak/,appimage/,deb/,rpm/}
├─ locales/                         # 见 06
├─ themes/{builtin/,schema/}        # 见 05
├─ icons/                           # 应用图标、文件类型图标源 SVG
├─ fixtures/                        # 测试压缩包（Git LFS），大文件由脚本确定性生成
├─ schemas/                         # 主题、CLI JSON 输出、配置的 JSON Schema
├─ scripts/                         # gen-shell-menus、i18n-check、build-icons、bench、release
├─ docs/                            # 设计文档与 ADR
├─ fuzz/                            # cargo-fuzz 目标（独立 workspace）：detect、zip、sevenz、tar、rar、mount、sanitize、encoding
├─ .github/workflows/{ci,release,nightly-fuzz,bench}.yml   # 当前已有 ci、release、nightly-fuzz
└─ SECURITY.md  CONTRIBUTING.md  CHANGELOG.md  LICENSE
```

## 2. 工具链

| 领域 | 工具 |
|---|---|
| Rust | stable（MSRV = 最新 stable − 2）、`cargo-nextest`、clippy（`-D warnings`）、rustfmt、`cargo-deny`、`cargo-audit`、`cargo-fuzz`（nightly）、criterion、sccache。**当前**：workspace `rust-version = 1.97`（Tauri 2.12 所需），CI 用 `cargo test`，nextest / deny / audit / criterion 尚未接入 |
| 前端 | Node 22 LTS、pnpm 10、Vite 7、TypeScript 5（strict）、Biome（lint + format）、stylelint、Vitest、Playwright。**当前**：pnpm 10.34.9、Vite 8、TypeScript 5.9、Tauri 2.12；lint 仅 `tsc`，Biome / stylelint / Playwright 尚未接入 |
| 桌面 | Tauri CLI 2.x、WiX 5、NSIS 3、MakeAppx / SignTool（Windows SDK）、Xcode 16+（Swift 扩展）、`create-dmg`、`appimagetool`、`flatpak-builder` |
| 质量 | lefthook 提交钩子、Conventional Commits、`git-cliff` 生成 CHANGELOG、Renovate |

## 3. 构建产物

| 平台 | 产物 | 内容 |
|---|---|---|
| Windows | `OmniZip_<ver>_x64.msix`、`_arm64.msix`、`OmniZip-Setup-<ver>-x64.exe`（NSIS）、`.msi`（WiX，企业静默部署）、便携 zip | `omnizip-desktop.exe`、`omnizip.exe`、`omnizip-mountd.exe`、`omnizip_shell.dll`（x64 + arm64）、WinFsp 可选组件引导 |
| macOS | `OmniZip-<ver>.dmg`（Universal）、`.pkg`（企业，可选）、Homebrew Cask | `.app` 内含 Finder Sync 扩展、FSKit 扩展、`omnizip` CLI、`omnizip-mountd` |
| Linux | `.deb`（amd64 / arm64）、`.rpm`、`.AppImage`、Flatpak（Flathub）、AUR PKGBUILD、tarball | 主程序、CLI、守护进程、`.desktop`、MIME、菜单文件、手册页 |

Tauri bundler 负责基础打包；MSIX、Sparse Package、Flatpak manifest 由 `scripts/release` 补充。

**当前实现（Windows）**：`pnpm bundle:windows` 用 `apps/desktop/src-tauri/bundle.windows.json` 叠加基础配置执行 `tauri build`：先构建前端，再由 `scripts/prepare-bundle.mjs` 以 release 构建 CLI、挂载守护进程 `omnizip-mountd.exe` 与 `omnizip_shell.dll`，前两者作为 sidecar（`externalBin`）、后者作为资源放入安装包（守护进程可能在运行，安装与卸载钩子把旧文件改名而不是覆盖），并用 `pack.ps1 -Sparse` 生成 Win11 一级菜单的菜单包 `omnizip_menu.msix`（Sparse Package，同样作为资源，安装时由钩子注册，见 07 §3.3；需要 Windows SDK）。菜单包用 `OMNIZIP_SIGN_PFX` / `OMNIZIP_SIGN_PASSWORD` 指定的证书签名，发布者取 `OMNIZIP_PUBLISHER`（构建桌面应用时同样读取，用于查找已注册的菜单包）；未设置时改用项目测试证书（公钥 `platform/windows/msix/omnizip-test.cer` 在仓库中、由程序内置；私钥由 `OMNIZIP_TEST_PFX` / `OMNIZIP_TEST_PASSWORD` 指定，或取开发机上 `new-test-cert.ps1` 的输出目录，绝不提交），都没有则不签名（安装后只有经典菜单）。压缩文件类型图标 `icons/filetypes/<扩展名>.ico` 由 `scripts/gen-filetype-icons.mjs`（`@resvg/resvg-js`）生成并提交到仓库，打包时复制为资源 `filetypes/`，安装钩子设为各文件类型的图标；右键菜单各项的图标 `icons/menu/*.ico` 由 `scripts/gen-menu-icons.mjs` 生成，同样复制为资源 `menu-icons/`（完整 MSIX 由 pack.ps1 放入包内）。NSIS 钩子见 `platform/windows/nsis/hooks.nsh`。这些只在打包时需要的设置不能放进 `tauri.conf.json`：Tauri 的构建脚本每次编译都会检查 sidecar 文件是否存在，干净检出（包括 CI）会因此无法编译。MSIX 由 `platform/windows/msix/pack.ps1` 依据 `AppxManifest.xml` 模板打包（MakeAppx，签名可选）；任务栏、开始菜单与文件类型图标由系统按像素尺寸从包内选取，因此脚本从 exe 图标原图生成各 `targetsize`（含 `altform-unplated`）与 `scale` 变体，并用 MakePri 生成单个 `resources.pri`（缺少时系统把 44×44 图标缩放后显示，模糊且与 exe 图标不一致）；包 Logo（StoreLogo）取图标圆角矩形内的正方形铺满，因为 App Installer 总把它放在系统强调色的方形底板上。打包应用在 AppData 中新建的文件夹默认被重定向到包的私有副本，资源管理器看不到（全新安装时"打开主题文件夹"报"位置不可用"），因此清单把 `%APPDATA%\OmniZip` 与 `%LOCALAPPDATA%\OmniZip` 排除在写入虚拟化之外（Windows 11 按目录排除，Windows 10 只能整体关闭 AppData 写入虚拟化），需受限能力 `unvirtualizedResources`；配置、主题与数据因此与 NSIS 安装位于同一位置，卸载时保留。WiX / MSI、ARM64 与 Linux 产物尚未接入。

**当前实现（macOS）**：在 Mac 上执行 `pnpm build`（即 `tauri build`）产出 `OmniZip.app` 与 `.dmg`（`target/release/bundle/macos/`、`dmg/`）。Tauri 在 macOS 上自动叠加 `apps/desktop/src-tauri/tauri.macos.conf.json`：打包目标改为 app 与 dmg，最低系统 12.0，合并 `platform/macos/Info.plist`（文档类型与类型导入，见 07 §4.2），并把 `icons/filetypes/<扩展名>.icns` 复制到 `Contents/Resources/`。`.icns` 与 `.ico` 由同一个 `scripts/gen-filetype-icons.mjs` 生成。只构建本机架构，未签名、未公证；Universal、Developer ID 签名与公证、Finder 扩展、CLI 与守护进程尚未接入。

## 4. CI 流水线（GitHub Actions）

### `ci.yml`（PR 与主干）

```
lint ──► unit ──► integration ──► e2e ──► visual ──► bundle-smoke
 │        │          │              │        │           │
 │        │          │              │        │           └─ 各平台产出安装包（不签名）
 │        │          │              │        └─ Playwright 截图回归（主题 × 语言）
 │        │          │              └─ tauri-driver（Windows / Linux）；macOS 走 Playwright 对接 dev server
 │        │          └─ CLI assert_cmd + fixtures；差分测试 vs 7-Zip / bsdtar
 │        └─ nextest + vitest，矩阵：windows-latest、macos-14、macos-13、ubuntu-22.04、ubuntu-22.04-arm
 └─ clippy、rustfmt、Biome、stylelint、i18n-check、cargo-deny、schema 校验
```

### 其他工作流

| 工作流 | 触发 | 内容 |
|---|---|---|
| `nightly-fuzz.yml` | 每日（主干 25 小时内无新提交则跳过） | 每个 fuzz target 1 小时；新崩溃自动开 issue 并附样本（保留 30 天）；每个 target 只保留最新的语料缓存 |
| `bench.yml` | 主干合并 | criterion 基准对比上一主干；回归 > 5% 标记并通知 |
| `release.yml` | 推送 `v*` tag，需人工批准 | 全平台构建 → 签名 / 公证 → 生成 updater manifest → GitHub Release（附 SBOM、校验和）→ 提交 winget / Homebrew / Flathub / AUR 更新 |

**用量控制（当前实现）**：仓库公开，GitHub 标准运行器不计费；以下做法让流水线在转为私有后也不浪费分钟数与存储——只改文档（`docs/**`、`*.md`、`LICENSE`）不触发 CI；同一分支的新推送取消进行中的运行；每个任务设超时（lint 20 分钟，测试与打包 40 分钟，默认 6 小时）；测试构建关闭调试信息（`CARGO_PROFILE_DEV_DEBUG=0`），编译更快、缓存更小；只有 main 保存构建缓存；fuzz 冒烟只在 Linux 跑；bundle-smoke 的安装包只在 main 与手动运行时上传，保留 1 天。

机密管理：Windows 签名走 Azure OIDC；Apple 用 App Store Connect API Key；minisign 私钥仅在发布 runner 的加密机密中，轮换计划每年一次。

## 5. 签名与公证

| 平台 | 方案 |
|---|---|
| Windows | Azure Trusted Signing（或 EV 证书 + HSM）；签名对象：所有 exe / dll / msix / msi；Sparse Package 与主程序同证书 |
| macOS | Developer ID Application 签名 app 与所有嵌入的扩展、CLI、守护进程；硬化运行时；`notarytool` 公证 + `stapler`；DMG 同样签名公证 |
| Linux | `.deb` / `.rpm` 仓库 GPG 签名；AppImage 嵌入 GPG 签名；Flathub 由平台签名 |

## 6. 自动更新

- Tauri updater：渠道 `stable` / `beta` / `nightly`；每渠道、平台、架构一份 `latest.json`（minisign 签名）；下载后校验签名再安装。
- 渠道差异：MSIX 走 App Installer / Store 更新；Linux 包管理器与 Flatpak 渠道在构建时关闭内置更新；AppImage 支持 zsync 增量。
- 体验：每日后台检查一次（可关闭）；静默下载，下次启动安装；Windows 安装时若 Explorer 持有 Shell DLL，按 07 §3.3 处理。
- 增量更新（bsdiff）列入 v1.x。

## 7. 测试策略

| 层级 | 范围 | 工具 | 目标 |
|---|---|---|---|
| 单元 | 核心库、路径净化、编码探测、ICU 文案、主题解析 | nextest、Vitest | 核心库行覆盖 ≥ 85% |
| 属性 | 任意数据 → 压缩 → 解压 round-trip；路径净化幂等 | proptest | 每格式 |
| 差分 | 与 7-Zip / bsdtar 比对列表与解压哈希 | 自研脚本 | 全 fixtures |
| 集成 | CLI 全命令、IPC 协议、守护进程 | assert_cmd | 三平台 |
| 端到端 | GUI 关键路径 12 条（打开、解压、压缩、密码、冲突、挂载、主题切换、语言切换…） | tauri-driver / Playwright | 三平台 |
| 视觉回归 | 主题 × 语言 × 关键界面 | Playwright | 差异 < 0.1% |
| 性能 | 启动、列表、解压 / 压缩吞吐、挂载延迟 | criterion、自研 | 不低于预算（见 02 §8、12 §10） |
| 模糊 | 全部解析器 | cargo-fuzz、AFL++ | 每周 ≥ 24 h |
| 手动矩阵 | 右键菜单、文件关联、挂载、安装 / 升级 / 卸载 | 发布清单 | 每次发布 |

## 8. 分支与协作

- Trunk-based：`main` 始终可发布；短生命周期特性分支；Conventional Commits。
- 评审：普通改动 1 人；`omnizip-core`、Shell 扩展、守护进程、安全相关 2 人。
- ADR 持续记录在 `docs/adr/`；错误码目录与 CLI JSON Schema 变更需同步更新文档。
- Issue 模板：bug（附诊断包）、格式支持请求、翻译问题、安全（私密通道）。

## 9. 版本与发布节奏

- `0.x` 内部迭代 → `1.0.0`；之后每 6–8 周一个 minor，patch 按需；beta 渠道提前 2 周。
- 发布清单：i18n 冻结 → 视觉回归通过 → 安全清单 → 三平台手测矩阵 → 签名验证 → 从上一版本升级验证 → 回滚方案（保留上一版本下载与 manifest）。
- 支持策略：最近两个 minor 版本接受安全修复。

## 10. 诊断

- "设置 → 高级 → 导出诊断包"：脱敏日志、配置、系统信息、集成状态（注册表 / plist / 菜单文件存在性）、后端健康（WinFsp / FSKit / FUSE）。
- 默认零遥测；可选崩溃报告见 09 §10。
