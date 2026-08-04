# IPMT

IPMT 是一个用 Rust、Ratatui 和 Crossterm 编写的 pi `models.json` 终端编辑器。它直接管理本机的自定义提供商与模型，并尽量保持配置对 pi 新版本的向前兼容。

## 能力

- 提供商与模型双栏浏览，支持 ID、名称、API 类型和地址的模糊搜索
- 快速新增、编辑、复制、删除，提供 OpenAI、Ollama、LM Studio、Anthropic 和 Google 模板
- 编辑推理、图像输入、上下文、输出上限、成本、thinking map、headers 和 compat
- 从 OpenAI、Anthropic、Google 风格的模型目录发现并批量导入模型
- 撤销/重做、删除确认、未保存退出保护和磁盘并发修改检测
- 保存前执行 schema 与语义校验，未知 JSON 字段和成本阶梯不会因普通表单编辑而丢失
- 原子保存、时间戳备份，并在 Unix 上将配置与备份权限设为 `0600`
- API key 默认遮罩；详情、校验和服务端错误中不会显示认证值
- 响应式布局：宽终端三栏，中等终端双栏，窄终端单栏
- 完整鼠标操作：点击选择、双击编辑、右键快速编辑、滚轮导航和弹窗按钮

IPMT 编辑的是 pi 的 `models.json`，不修改内置模型缓存 `models-store.json`。pi 每次打开 `/model` 时会重新载入该文件。

## 安装

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
| `s`、`Ctrl+S` | 校验并保存 |
| `Ctrl+Z` / `Ctrl+Y` | 撤销 / 重做 |
| `v` | 查看校验结果 |
| `r` | 从磁盘重新载入 |
| `F1`、`?` | 帮助 |
| `q` | 退出 |

鼠标左键用于选择列表项、字段和底部操作，双击提供商或模型可编辑，右键列表项可直接编辑，滚轮用于移动列表选择或滚动弹窗。终端启用鼠标捕获后，如需选择终端文本，通常按住 `Shift` 再拖动。

编辑表单、提供商模板和远程发现等弹窗采用上下两个焦点区：`Tab` / `Shift+Tab` 直接在内容区与按钮区之间切换；内容区使用 `Up/Down` 移动，位于末项时继续按 `Down` 进入按钮区；按钮区使用 `Left/Right` 选择，按 `Up` 返回内容区，`Enter` / `Space` 激活。表单中 `Ctrl+S` 可随时应用，API key 可用 `F3` 临时显示；鼠标点击布尔值或枚举值也可直接切换。点击提供商或模型列表框的空白区域也会切换到对应面板。

## 远程模型发现

发现功能根据提供商 API 类型请求模型目录：

- `openai-completions` / `openai-responses`: `<baseUrl>/models`
- `anthropic-messages`: `<baseUrl>/v1/models`，若地址已以 `/v1` 结尾则使用 `<baseUrl>/models`
- `google-generative-ai`: `<baseUrl>/models`

认证解析顺序与 pi 一致：`auth.json`、已知提供商环境变量、`models.json` 中的 `apiKey`。支持 `$VAR`、`${VAR}`、`$$` 和 `$!`；为了避免编辑器隐式执行任意代码，发现功能不会执行 `!command` 类型的值。

## 保存策略

1. 重新比较磁盘内容，发现外部修改时停止保存。
2. 将当前磁盘版本写入 `models.json.bak.<timestamp>`。
3. 在同一目录写入并同步权限为 `0600` 的临时文件。
4. 原子替换目标文件并同步目录。

普通表单只合并它负责的字段。根对象、提供商、模型中的未知字段，以及 `cost.tiers` 等高级配置会保留。

## 开发验证

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo run -- --check
```

项目针对 pi 当前支持的四种 `models.json` API 类型进行校验：`openai-completions`、`openai-responses`、`anthropic-messages` 和 `google-generative-ai`。
