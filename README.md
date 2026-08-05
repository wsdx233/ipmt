# IPMT

IPMT 是一个用 Rust、Ratatui 和 Crossterm 编写的 pi `models.json` 终端编辑器。它直接管理本机的自定义提供商与模型，并尽量保持配置对 pi 新版本的向前兼容。

## 能力

- 提供商与模型双栏浏览，支持 ID、名称、API 类型和地址的模糊搜索
- 快速新增、编辑、复制、删除，提供 OpenAI、Ollama、LM Studio、Anthropic 和 Google 模板
- 编辑推理、图像输入、上下文、输出上限、成本、thinking map、headers 和 compat
- 从 OpenAI、Anthropic、Google 风格的模型目录发现并批量导入模型
- 自动合并 `sub2api` 与 `router-for-me/models` 最新模型数据，支持按模型 ID 搜索，并带上下文、推理、视觉、价格和思考级别映射快速导入
- 选中模型后通过 pi 发起最小连通性测试，在弹窗中查看模型响应或错误信息
- 撤销/重做、删除确认、未保存退出保护和磁盘并发修改检测
- 保存前执行 schema 与语义校验，未知 JSON 字段和成本阶梯不会因普通表单编辑而丢失
- 原子保存、时间戳备份，自动保留最近 20 份；Unix 上配置与备份权限为 `0600`，备份目录为 `0700`
- API key 默认遮罩；详情、校验和服务端错误中不会显示认证值
- 响应式布局：宽终端三栏，中等终端双栏，窄终端单栏
- 完整鼠标操作：点击选择、双击编辑、右键快速编辑、滚轮导航和弹窗按钮

IPMT 编辑的是 pi 的 `models.json`，不修改内置模型缓存 `models-store.json`。pi 每次打开 `/model` 时会重新载入该文件。

## 安装

### Linux 一键安装

复制并执行下面这条命令，即可从最新 GitHub Release 自动下载、校验并安装到 `~/.local/bin`（支持 Linux x86_64）：

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://raw.githubusercontent.com/wsdx233/ipmt/master/install.sh | sh
```

脚本会在需要时把 `~/.local/bin` 加入 shell 配置。重新打开终端后可直接运行 `ipmt`。也可通过 `IPMT_INSTALL_DIR` 指定安装目录：

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://raw.githubusercontent.com/wsdx233/ipmt/master/install.sh | IPMT_INSTALL_DIR="$HOME/bin" sh
```

### Windows 一键安装

在 PowerShell 中复制并执行下面这条命令，即可从最新 GitHub Release 自动下载、校验并安装 Windows x86_64 版本：

```powershell
irm https://raw.githubusercontent.com/wsdx233/ipmt/master/install.ps1 | iex
```

默认安装到 `%LOCALAPPDATA%\Programs\ipmt\bin`，脚本会自动将该目录加入当前用户的 `PATH`。重新打开终端后可直接运行 `ipmt`。也可通过 `IPMT_INSTALL_DIR` 指定安装目录：

```powershell
$env:IPMT_INSTALL_DIR = "$HOME\bin"; irm https://raw.githubusercontent.com/wsdx233/ipmt/master/install.ps1 | iex
```

从源码安装：

```bash
cargo install --path .
```

也可以直接构建：

```bash
cargo build --release
./target/release/ipmt
```

## 配置路径

默认路径与 pi 保持一致：

```text
~/.pi/agent/models.json
```

设置 `PI_CODING_AGENT_DIR` 后，IPMT 使用 `$PI_CODING_AGENT_DIR/models.json`。也可显式指定文件，适合先在副本上试用：

```bash
ipmt --file ./models.test.json
```

## 命令行

```text
ipmt [OPTIONS]

--file <PATH>  编辑指定 models.json
--check        只校验并输出脱敏摘要，不启动 TUI
--read-only    可浏览和远程发现，但禁止修改与保存
--no-backup    普通保存不创建备份
```

当检测到文件被其他进程修改时，强制覆盖前仍会创建备份，即使使用了 `--no-backup`。

## 常用操作

| 按键 | 操作 |
|---|---|
| `Up/Down`、`j/k` | 移动选择 |
| `Left/Right`、`Tab` | 切换提供商与模型 |
| `/` | 模糊搜索 |
| `n` | 在当前栏新增 |
| `p` / `m` | 新增提供商 / 模型 |
| `Enter`、`e` | 编辑当前项 |
| `d`、`Delete` | 删除当前项 |
| `c` | 复制当前项并生成唯一 ID |
| `f` | 从当前提供商发现模型 |
| `i`（模型栏） | 搜索已知模型并按能力参数快速导入 |
| `t`（模型栏） | 通过 pi 测试当前模型，显示响应或错误 |
| `s`、`Ctrl+S` | 校验并保存 |
| `Ctrl+Z` / `Ctrl+Y` | 撤销 / 重做 |
| `v` | 查看校验结果 |
| `r` | 从磁盘重新载入 |
| `F1`、`?` | 帮助 |
| `q` | 退出 |

鼠标左键用于选择列表项、字段和底部操作，双击提供商或模型可编辑，右键列表项可直接编辑，滚轮用于移动列表选择或滚动弹窗。终端启用鼠标捕获后，如需选择终端文本，通常按住 `Shift` 再拖动。

编辑表单、提供商模板和远程发现等弹窗采用上下两个焦点区：`Tab` / `Shift+Tab` 直接在内容区与按钮区之间切换；内容区使用 `Up/Down` 移动，位于末项时继续按 `Down` 进入按钮区；按钮区使用 `Left/Right` 选择，按 `Up` 返回内容区，`Enter` / `Space` 激活。表单中 `Ctrl+S` 可随时应用，API key 可用 `F3` 临时显示；鼠标点击布尔值或枚举值也可直接切换。点击提供商或模型列表框的空白区域也会切换到对应面板。

提供商预设、远程发现和已知模型导入等选择页面在内容区可直接按 `Enter` 执行主操作；也可以按 `Tab` 移动到下方按钮后确认。

## 远程模型发现

发现功能根据提供商 API 类型请求模型目录：

- `openai-completions` / `openai-responses`: `<baseUrl>/models`
- `anthropic-messages`: `<baseUrl>/v1/models`，若地址已以 `/v1` 结尾则使用 `<baseUrl>/models`
- `google-generative-ai`: `<baseUrl>/models`

认证解析顺序与 pi 一致：`auth.json`、已知提供商环境变量、`models.json` 中的 `apiKey`。支持 `$VAR`、`${VAR}`、`$$` 和 `$!`；为了避免编辑器隐式执行任意代码，发现功能不会执行 `!command` 类型的值。

已知模型快速导入会合并 `sub2api` 的 `model_prices_and_context_window.json` 与 `router-for-me/models`。相同模型 ID 优先使用 sub2api 的能力、上下文和价格；仅出现在 router-for-me 中的模型作为补充，因此快捷导入和 `f` 发现后的参数匹配都能覆盖更多模型。任一目录暂时不可用时仍会使用另一个目录。sub2api 的每 token 价格会转换为 pi 使用的每百万 token 价格；推理能力标记会转换为 `reasoning` 和 `thinkingLevelMap`。同时支持 `max` 与 `xhigh` 时，Claude 模型映射为 `{"xhigh":"max"}`，GPT 和其他模型映射为 `{"xhigh":"xhigh"}`。

在 `f` 发现结果页面按 `/` 可进入模型 ID 筛选，输入内容时实时过滤；`Enter` 或 `Esc` 结束输入并保留筛选结果。筛选状态下的 Space、`a`、`x` 操作当前可见模型。

## 模型测试

切换到模型栏并按 `t`，IPMT 会创建当前内存配置的临时快照，并调用 `pi --print --no-session --no-tools` 向选中模型发送最小测试提示。未保存的配置和通过 `--file` 打开的配置也可直接测试；同目录的 `auth.json` 会复制到临时 Pi 配置目录，环境变量与 `models.json` 中的认证方式仍由 pi 解析。

测试在后台运行，不会阻塞 TUI，最长等待 60 秒。完成后弹窗显示模型文本响应，或显示 pi 的 stderr 与退出错误；较长内容可用方向键、PageUp/PageDown 或鼠标滚轮查看。该功能要求 `pi` 命令已安装并位于 `PATH` 中。

## 保存策略

1. 重新比较磁盘内容，发现外部修改时停止保存。
2. 将当前磁盘版本写入同目录的 `.backup/models.json.bak.<timestamp>`，并只保留该配置最近 20 份备份。
3. 在同一目录写入并同步权限为 `0600` 的临时文件。
4. 原子替换目标文件并同步目录。

使用 `--file` 指定其他配置时，备份同样写入该配置所在目录的 `.backup/`。Unix 上该目录权限为 `0700`，备份文件权限为 `0600`。

普通表单只合并它负责的字段。根对象、提供商、模型中的未知字段，以及 `cost.tiers` 等高级配置会保留。

## 开发验证

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo run -- --check
```

项目针对 pi 当前支持的四种 `models.json` API 类型进行校验：`openai-completions`、`openai-responses`、`anthropic-messages` 和 `google-generative-ai`。

## 致谢

感谢 Linux.do 社区提供的分享与交流平台
