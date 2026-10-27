# 08 · 命令行工具 `omnizip`

## 1. 定位

- 独立二进制，静态链接核心库，**不依赖 GUI 进程**，可在服务器、CI、容器中运行。
- 两类用户：终端用户（可读表格、进度条、交互提示）与脚本（`--json`、稳定退出码、`--no-input`）。
- 与 GUI 共享 `config.toml` 与预设；GUI 的压缩 / 解压对话框提供"复制为命令行"。
- 可选短别名 `oz`。

## 2. 命令总览

```
omnizip <command> [options] [args]

  extract    x    解压（保留目录结构，默认智能解压）
  flat       e    解压到单层目录（不保留路径）
  create     a    创建压缩包 / 向已有压缩包添加
  update     u    仅添加新文件与更新较新的文件
  delete     d    从压缩包删除条目
  rename     rn   重命名条目
  list       l    列出内容
  test       t    测试完整性
  convert         转换格式（rar → 7z、zip → tar.zst …）
  mount / unmount / mounts   虚拟挂载（见 12-mount.md）
  info       i    显示压缩包信息（格式、方法、固实、加密、注释）
  hash            计算文件或条目哈希（crc32 / md5 / sha1 / sha256 / blake3）
  bench           在当前机器上基准测试各算法
  gui             打开图形界面（可带文件）
  completions     生成 Shell 补全脚本
  config          查看 / 修改配置与预设
  formats         列出当前构建支持的格式与能力
```

- 短别名沿用 7-Zip 习惯便于迁移；参数采用 GNU 风格（`--level 9`）。
- 兼容映射：常见 7-Zip 参数（`-mx=9`、`-p<pw>`、`-o<dir>`、`-y`）被接受并在 `--help` 中标注"兼容"。

## 3. 命令示例

### 解压

```bash
omnizip x backup.7z                          # 智能解压到当前目录
omnizip x backup.7z -o ./out --path full     # 完整路径
omnizip x a.zip b.rar c.tar.zst --each       # 多个压缩包各自独立目录
omnizip x secret.7z -p                       # 交互式输入密码
omnizip x secret.7z --password-file ~/.pw    # 从文件读密码（权限需 600）
omnizip x big.zip --include '*.jpg' --exclude 'thumbs/**'
omnizip x a.zip --overwrite skip|all|older|rename
omnizip x legacy.zip --encoding gbk          # 指定文件名编码
omnizip x data.tar.gz --stdout | tar -x      # 流式输出
curl -s https://x/a.tar.zst | omnizip x - -o out   # 从 stdin
```

### 创建

```bash
omnizip a project.7z ./src --level 9 --solid --threads 8
omnizip a photos.zip *.jpg --level 3
omnizip a backup.tar.zst ~/home --exclude-from .backupignore --follow-symlinks
omnizip a secret.7z docs/ -p --encrypt-headers
omnizip a big.7z data/ --volume 4g
omnizip a out.zip ./dist --preset fast-share
omnizip a - ./dir --format tar.gz > out.tgz  # 输出到 stdout
omnizip u backup.zip ./src --delete-missing  # 同步模式
```

### 列出与检查

```bash
omnizip l a.zip                              # 表格
omnizip l a.zip --tree
omnizip l a.zip 'src/**/*.rs'                # glob 过滤
omnizip l a.zip --json | jq '.entries[] | select(.size > 1e6)'
omnizip l a.zip --porcelain                  # 制表符分隔、列稳定
omnizip t *.7z --json
omnizip i a.rar
omnizip hash a.zip --entry 'docs/report.pdf' --algo sha256
```

### 转换与挂载

```bash
omnizip convert old.rar --to 7z --level 7 --delete-source
omnizip convert *.zip --to tar.zst --out-dir ./converted
omnizip mount src.7z                         # 默认挂载点并打印路径
omnizip mount src.7z --exec -- tokei {mount} # 临时挂载执行命令后卸载
omnizip unmount --all
```

## 4. 通用选项

| 选项 | 说明 |
|---|---|
| `-o, --output <dir>` | 输出目录 / 文件 |
| `-p, --password [pw]` | 裸 `-p` 交互提示；行内给值时警告（进程列表可见） |
| `--password-file <f>` / `--password-env <VAR>` | 脚本友好的密码来源 |
| `-y, --yes` / `--no-input` | 全部默认确认 / 禁止任何交互（非 TTY 默认） |
| `-q, --quiet` / `-v, --verbose` | 输出级别（`-vv` 调试） |
| `--json` / `--porcelain` | 机器可读输出 |
| `--progress auto\|bar\|plain\|none` | 进度显示方式 |
| `--color auto\|always\|never` | 遵循 `NO_COLOR`、`CLICOLOR_FORCE` |
| `--lang <tag>` | 覆盖输出语言 |
| `--threads <n>` | 0 = 自动 |
| `--config <file>` / `--preset <name>` | 配置与预设 |
| `--dry-run` | 只显示将执行的操作 |
| `--log-file <f>` | 详细日志 |
| `--encoding <name>` / `--encoding auto` | 文件名编码 |
| `--allow-high-ratio` / `--max-size <n>` | 安全策略覆盖（见 09） |

## 5. 输出规范

- **人类模式**：自适应宽度表格；`indicatif` 多任务进度条；结束时一行摘要（文件数、字节、耗时、速度、警告数）。
- **`--json`**：非流式命令输出单个 JSON 对象；配合 `--progress` 或长任务时输出 NDJSON 事件流，每行一个事件：`{"type":"progress",…}`、`{"type":"entry",…}`、`{"type":"warning",…}`、`{"type":"done",…}`。Schema 发布在 `schemas/cli/*.json`，字段只增不删。
- **`--porcelain`**：制表符分隔、列顺序固定，适合 `awk` / `cut`。
- **错误**到 stderr，格式：`omnizip: error[E1203]: 密码错误（a.7z）`；`--json` 模式下 stderr 同样输出 JSON。
- 非 TTY 自动关闭颜色与进度条；`--progress plain` 每秒输出一行百分比，便于 CI 日志。
- Windows 下 stdout / stdin 处理二进制时切换为二进制模式，控制台输出启用 UTF-8。

## 6. 退出码

| 码 | 含义 |
|---|---|
| 0 | 成功 |
| 1 | 完成但有警告（跳过、损坏文件已保留） |
| 2 | 用法错误 |
| 3 | 压缩包不存在或不可读 |
| 4 | 格式不支持或无法识别 |
| 5 | 压缩包损坏 |
| 6 | 密码错误或需要密码 |
| 7 | 目标写入失败（权限、空间） |
| 8 | 安全策略拒绝（Zip Slip、炸弹阈值） |
| 9 | 用户取消（SIGINT / Ctrl+C） |
| 10 | 缺少可选依赖（libarchive） |
| 20–24 | 挂载相关（见 12） |
| 70 | 内部错误（附带诊断提示） |

## 7. 交互与信号

- 密码：TTY 下无回显提示；`--no-input` 下直接以 6 退出。
- 冲突：TTY 询问 `[o]覆盖 [s]跳过 [r]重命名 [O]全部覆盖 [S]全部跳过 [q]退出`；非 TTY 默认跳过并以 1 退出，除非给定 `--overwrite`。
- Ctrl+C：优雅取消，删除未完成的输出文件（`--keep-partial` 保留），以 9 退出；第二次 Ctrl+C 立即退出。
- Windows：处理 `CTRL_CLOSE_EVENT`；manifest 启用 `longPathAware`。

## 8. 配置与预设

- 共享 `config.toml` 的 `[cli]` 与 `[presets.*]` 段：

```toml
[cli]
default_format = "7z"
progress = "auto"

[presets.fast-share]
format = "zip"
level = "fast"
exclude = [".DS_Store", "Thumbs.db", "node_modules/**"]

[presets.archive]
format = "7z"
level = "ultra"
solid = true
encrypt_headers = true
```

- `omnizip config get cli.default_format`、`omnizip config set presets.x.level best`。
- 环境变量：`OMNIZIP_CONFIG`、`OMNIZIP_THREADS`、`OMNIZIP_LANG`、`OMNIZIP_NO_UPDATE_CHECK`。

## 9. 安装与 Shell 环境

| 平台 | PATH | 补全 | 手册页 |
|---|---|---|---|
| Windows | 安装器可选"添加到 PATH"；MSIX 通过 App Execution Alias 自动提供 `omnizip.exe` | PowerShell：`omnizip completions powershell \| Out-String \| Invoke-Expression`，设置页可一键写入 `$PROFILE` | `omnizip help` |
| macOS | Homebrew 自动；.app 内置 `Contents/MacOS/omnizip`，设置页"安装命令行工具"创建 `/usr/local/bin/omnizip` 符号链接 | zsh / bash / fish | `man omnizip` |
| Linux | 包管理器安装到 `/usr/bin`；AppImage `--install-cli` 创建 `~/.local/bin` 包装 | bash / zsh / fish / elvish / nushell | `man omnizip` |

- 补全由 `clap_complete` 生成；动态补全压缩包内路径（`omnizip x a.zip --include <TAB>`，上限 2000 项）。
- 手册页由 `clap_mangen` 生成 `omnizip.1` 与各子命令页。
- `--help` 文案本地化（Fluent），`LANG=C` 时为英文。

## 10. 脚本示例

```bash
# CI：打包并校验
omnizip a dist.tar.zst ./dist --level best --threads 0 && omnizip t dist.tar.zst

# 批量 rar → 7z
for f in *.rar; do omnizip convert "$f" --to 7z --level better --delete-source; done

# 找出所有包里 > 100 MB 的文件
omnizip l *.zip --json | jq -r '.archive as $a | .entries[] | select(.size > 1e8) | "\($a)\t\(.path)"'
```

```powershell
# PowerShell：按日期归档日志
Get-ChildItem .\logs\*.log | Where-Object LastWriteTime -lt (Get-Date).AddDays(-30) |
  ForEach-Object { omnizip a "archive\$($_.BaseName).7z" $_.FullName --level ultra --delete-source }
```

## 11. 测试

- `assert_cmd` + fixtures：每个子命令覆盖成功 / 错误 / 非 TTY / JSON 快照。
- 跨平台 CI：Windows、macOS、Linux；长路径、Unicode 参数、stdin / stdout 二进制安全。
- JSON Schema 校验输出；退出码表为契约测试。
