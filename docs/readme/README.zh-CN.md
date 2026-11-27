# Morpheus

[English](../../README.md) | [Português](./README.pt-BR.md) | [Español](./README.es.md) | [Русский](./README.ru.md) | **中文** | [日本語](./README.ja.md)

<img width="1697" height="927" alt="Morpheus 终端界面" src="https://github.com/user-attachments/assets/7b3404d6-acd3-4f5e-a32a-5b941be99c9e" />

Morpheus 是一个运行在终端中的通用编码智能体（coding agent）。它提供软件工程工具，以及可与图形应用程序交互的 *computer tools*。

## 为什么选择 Morpheus？

Morpheus 并不打算取代成熟的 coding agent。它的定位是作为编排层与它们协同工作。以下是使用它的理由：

- **多智能体协作**：在同一 TUI 中运行不同的前沿智能体（Claude、Gemini、Codex、Cline 等），打造共享工作区，让它们通过单一界面进行沟通。
- **适合实验**：非常适合喜欢尝试不同智能体并融合各自优势，而不是绑定单一生态的开发者。
- **Manager 模式**：继续使用你现有的智能体 harness，同时在其之上增加编排能力，尤其适合同时处理多个较小任务时使用。
- **原生 TUI 体验**：直接在界面中配置 provider、模型和运行模式，几乎无需手动配置。
- **模块化架构**：面向开发工具、TUI 组件、SDK 和记忆召回的独立、可复用的 crates。

## 与其他 TUI 编排器的区别

| 特性                                         | Morpheus                                        | 其他 TUI 编排器            |
| ------------------------------------------ | ----------------------------------------------- | --------------------- |
| **多智能体分屏**                                 | 在分屏窗格中同时运行多个外部智能体，每个都有自己的会话、模型和可选的 git worktree | 通常聚焦单一智能体，或需要切换上下文    |
| **基于 accessibility tree 的 computer tools** | 通过结构化表示读取并操作图形界面，仅在必要时回退到截图                     | 通常依赖截图，或完全缺少 GUI 交互能力 |
| **基于 Rust 的 TUI**                          | 使用 Ratatui 从零构建，交互设计受 OpenCode 启发               | 往往是基于 Web 的终端或优化不足的界面 |
| **应用内配置**                                  | Provider、模型和模式直接在 TUI 中配置                       | 通常需要外部配置文件或 CLI 参数    |
| **模块化 crates**                             | 开发工具、TUI 组件、SDK 和记忆召回都是独立、可复用的库                 | 组件紧密耦合的单体架构           |
| **共享工作区**                                  | 多个智能体通过单一界面协作，并协调任务分派                           | 智能体通常各自独立工作           |

## 主要特性

- **多智能体模式（`/agents`）**：让外部智能体（Gemini、Claude、Codex、Cline 等）与主智能体一起在分屏窗格中运行，全部可以通过同一个提示词触达。每个智能体保留自己的会话、模型和可选的 git worktree，由主智能体统一协调，像一个团队一样分派任务、收集报告。
- **基于 accessibility tree 的 computer tools**：智能体通过结构化、紧凑的表示形式读取并操作图形界面，仅在必要时才回退到截图。处理的图像更少，消耗的 token 更少。
- **用 Rust 从零构建的 TUI**（Ratatui），交互设计受 OpenCode 启发。Provider、模型和运行模式都可以直接在界面中配置，几乎零手动配置。
- **独立的 crates**：开发工具被拆分为与应用解耦的 crates，可以作为库在 Morpheus 之外复用。

## 安装

已在 Linux 和 Windows 上测试。macOS 尚未测试；如果遇到问题，请[提交 issue](https://github.com/aidreamy/morpheus/issues)，并尽可能详细描述。

**Linux 和 macOS**（在 Windows 上也可通过 Git Bash / WSL 使用）：

```sh
curl -fsSL https://raw.githubusercontent.com/aidreamy/morpheus/main/download.sh | bash
```

**Windows（PowerShell）：**

```powershell
irm https://raw.githubusercontent.com/aidreamy/morpheus/main/download.ps1 | iex
```

> 注意：`curl | bash` 命令在 PowerShell 或 CMD 中**无法**使用，因为 Windows 没有原生的 `bash`。请使用上面的 PowerShell 脚本；Git Bash 和 WSL 同样可用。

可选变量：`MORPHEUS_BIN_DIR`（安装目录）、`MORPHEUS_VERSION`（指定版本）和 `MORPHEUS_VARIANT`（`slim` 为默认，`slim-embed` 含基于 fastembed 的本地 RAG，`slim-speak` 为完整语音构建）。`slim-embed` 构建目前仅提供 Linux x64 和 Windows x64 版本；macOS 提供 `slim` 和 `slim-speak`。在 PowerShell 中，请在运行命令前将它们设置为 `$env:MORPHEUS_VERSION = "v0.1.0"`。

### 语音输出

`slim-speak` 是完整的语音构建：通过 Kokoro 进行本地语音合成，并启用 `speak-phonemize` 前端，因此葡萄牙语（巴西）和其他没有内置词典的语言也可用，而不仅限于英语和中文。

```sh
export MORPHEUS_VARIANT=slim-speak
curl -fsSL https://raw.githubusercontent.com/aidreamy/morpheus/main/download.sh | bash
```

压缩包还包含额外的 `morpheus-ttsd` 二进制文件，应用会将其作为同级进程启动；请将其与 `morpheus` 放在同一目录中。在 Linux 上，播放需要 `paplay`、`pw-play`、`aplay`、`play` 或 `ffplay` 之一；macOS 使用内置的 `afplay`。

语音模型未内置：请从 [sherpa-onnx 模型发布](https://github.com/k2-fsa/sherpa-onnx/releases/tag/tts-models)下载 Kokoro 包，并解压到 `~/.cache/morpheus/models/`（或将 `MORPHEUS_KOKORO_MODEL_DIR` 指向解压目录）——默认语音使用 v1.1，巴西葡萄牙语语音使用 v1.0。

> **Windows:** 暂不支持语音输出。语音守护进程通过 Unix domain socket 通信，并使用 Unix 进程 API，因此 `slim-speak` 仅发布 Linux x64 和 macOS（x64 与 arm64）版本。Windows 提供 `slim` 和 `slim-embed`。

安装完成后，在终端运行 `morpheus`。如果安装目录不在你的 `PATH` 中，脚本会自动输出添加它的命令。安装过程中遇到任何问题，请提交 issue，我们会很乐意帮忙。

## Crates

| Crate                                                           | 描述                                        |
| --------------------------------------------------------------- | ----------------------------------------- |
| [`morpheus-agents`](../../crates/morpheus-agents/README.md)     | 子智能体集成：ACP 客户端 harness、会话状态和类型化事件流 |
| [`morpheus-tools`](../../crates/morpheus-tools/README.md)       | 开发工具与 computer tools                      |
| [`morpheus-tui`](../../crates/morpheus-tui/README.md)           | TUI 组件库：markdown/diff 渲染和布局，终端界面的组件层      |
| [`morpheus-sdk`](../../crates/morpheus-sdk/README.md)           | 可复用的智能体基础组件：补丁引擎、工具调用解析、LSP 客户端和 AST 智能分析 |
| [`morpheus-recall`](../../crates/morpheus-recall/README.md)     | 记忆与上下文召回                                  |
| [`morpheus-whatsapp`](../../crates/morpheus-whatsapp/README.md) | WhatsApp Web（多设备）集成：配对、规范化事件和类型化发送        |
| [`morpheus-onnx`](../../crates/morpheus-onnx/README.md)         | 由 `onnx` feature 启用的 ONNX 决策引擎            |

## 路线图

- **Laya/ONNX 决策引擎**：由 `morpheus-onnx` crate 提供的可选 `onnx` feature；参见 [crate 指南](../onnx/onnx.md)。
- **基于图的 harness 编排**：一种用于协调多个 harness 的新会话模式。

## 许可证

GPL-3.0。参见 [LICENSE](../../LICENSE)。
