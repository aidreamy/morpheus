# Morpheus

[English](../../README.md) | [Português](./README.pt-BR.md) | [Español](./README.es.md) | [Русский](./README.ru.md) | [中文](./README.zh-CN.md) | **日本語**

<img width="1910" height="1064" alt="Morpheus のターミナル画面" src="https://github.com/user-attachments/assets/a80fcff1-068a-44f8-b43d-3e7d7697ecd6" />

Morpheus はターミナル上で動作する汎用コーディングエージェントです。ソフトウェア開発ツールに加え、グラフィカルアプリケーションと対話できる *computer tools* を備えています。

## なぜ Morpheus なのか？

Morpheus は確立された coding agent を置き換えることを目的としていません。オーケストレーション層として、それらと並んで動作するよう設計されています。利用する理由は次のとおりです：

- **マルチエージェント協調**：さまざまな最先端エージェント（Claude、Gemini、Codex、Cline など）を同じ TUI 内で実行し、単一のインターフェースを通じてコミュニケーションできる共有ワークスペースを作ります。
- **実験しやすい**：単一のエコシステムに縛られるのではなく、さまざまなエージェントを試して強みを組み合わせたい開発者に最適です。
- **マネージャーモード**：既存のエージェント harness を使い続けながら、その上にオーケストレーションを追加できます。複数の小さなタスクを同時に進める際に特に便利です。
- **ネイティブな TUI 体験**：プロバイダー、モデル、動作モードをインターフェース上で直接設定でき、手動設定は最小限で済みます。
- **モジュール式アーキテクチャ**：開発ツール、TUI コンポーネント、SDK、メモリ recall のための独立した再利用可能な crates。

## 他の TUI オーケストレーターとの違い

| 機能                                        | Morpheus                                                           | 他の TUI オーケストレーター                        |
| ----------------------------------------- | ------------------------------------------------------------------ | --------------------------------------- |
| **マルチエージェント分割ペイン**                        | 複数の外部エージェントを分割ペインで同時に実行し、それぞれが独自のセッション、モデル、オプションの git worktree を持つ | 通常は単一エージェントに集中するか、コンテキストの切り替えが必要        |
| **accessibility tree による computer tools** | 構造化された表現を通じてグラフィカル UI を読み取り操作し、必要な場合のみスクリーンショットにフォールバック            | 通常はスクリーンショットに依存するか、GUI 操作をまったく持たない      |
| **Rust 製の TUI**                           | Ratatui でゼロから構築され、OpenCode の UX に着想                                | 多くの場合 Web ベースのターミナル、または最適化が不十分なインターフェース |
| **アプリ内設定**                                | プロバイダー、モデル、モードを TUI 内で直接設定                                         | 多くの場合、外部設定ファイルや CLI フラグが必要              |
| **モジュール式 crates**                         | 開発ツール、TUI ウィジェット、SDK、メモリ recall を独立した再利用可能なライブラリとして提供              | コンポーネントが密結合したモノリシックなアーキテクチャ             |
| **共有ワークスペース**                             | 複数のエージェントが単一のインターフェースを通じて協働し、タスク配分を調整                              | エージェントは通常それぞれ孤立して動作                     |

## 主な特徴

- **マルチエージェントモード（`/agents`）**：外部エージェント（Gemini、Claude、Codex、Cline など）をメインエージェントと一緒に分割ペインで実行でき、すべて同じプロンプトから操作できます。各エージェントは自分のセッション・モデル・オプションの git worktree を持ち、メインエージェントがチームとしてタスクを振り分け、レポートを回収します。
- **accessibility tree による computer tools**：エージェントは構造化されたコンパクトな表現を通じてグラフィカル UI を読み取り操作し、スクリーンショットはフォールバックとしてのみ使用します。処理する画像が減り、トークン消費も削減されます。
- **Rust でゼロから構築された TUI**（Ratatui）。UX は OpenCode に着想を得ています。プロバイダー、モデル、動作モードの設定はすべてインターフェース上で完結し、手動設定はほぼ不要です。
- **独立した crates**：開発ツールはアプリ本体から切り離された crates として提供され、Morpheus の外でもライブラリとして再利用できます。

## インストール

Linux と Windows でテスト済み。macOS は未テストです。問題が発生した場合は、できるだけ詳細を添えて [issue を作成](https://github.com/aidreamy/morpheus/issues)してください。

**Linux および macOS**（Windows では Git Bash / WSL でも動作します）:

```sh
curl -fsSL https://raw.githubusercontent.com/aidreamy/morpheus/main/download.sh | bash
```

**Windows（PowerShell）:**

```powershell
irm https://raw.githubusercontent.com/aidreamy/morpheus/main/download.ps1 | iex
```

> 注意: `curl | bash` コマンドは PowerShell や CMD では**動作しません**。Windows にはネイティブの `bash` がないためです。上記の PowerShell スクリプトを使用してください。Git Bash と WSL でも動作します。

オプション変数: `MORPHEUS_BIN_DIR`（インストール先ディレクトリ）、`MORPHEUS_VERSION`（特定バージョン）、`MORPHEUS_VARIANT`（デフォルトは `slim`、fastembed によるローカル RAG 付きは `slim-embed`）。`slim-embed` ビルドは現在 Linux x64 と Windows x64 のみ提供され、macOS には `slim` が配布されます。PowerShell では、コマンド実行前に `$env:MORPHEUS_VERSION = "v0.1.0"` のように設定してください。

インストール後、ターミナルで `morpheus` を実行してください。インストールディレクトリが `PATH` にない場合は、スクリプトが追加用のコマンドを表示します。インストールで問題があれば issue を作成してください。喜んでお手伝いします。

## Crates

| Crate                                                           | 説明                                                                 |
| --------------------------------------------------------------- | ------------------------------------------------------------------ |
| [`morpheus-agents`](../../crates/morpheus-agents/README.md)     | サブエージェント統合：ACP クライアントハーネス、セッション状態、型付きイベントストリーム |
| [`morpheus-tools`](../../crates/morpheus-tools/README.md)       | 開発ツールと computer tools                                              |
| [`morpheus-tui`](../../crates/morpheus-tui/README.md)           | TUI ウィジェットライブラリ：markdown/diff のレンダリングとレイアウト、ターミナル UI を構成するコンポーネント群 |
| [`morpheus-sdk`](../../crates/morpheus-sdk/README.md)           | エージェント向けの再利用可能な基盤：パッチエンジン、ツールコール解析、LSP クライアント、AST インテリジェンス         |
| [`morpheus-recall`](../../crates/morpheus-recall/README.md)     | メモリとコンテキストの再呼び出し                                                   |
| [`morpheus-whatsapp`](../../crates/morpheus-whatsapp/README.md) | WhatsApp Web（マルチデバイス）統合：ペアリング、正規化イベント、型付き送信                        |
| [`morpheus-onnx`](../../crates/morpheus-onnx/README.md)         | `onnx` feature で有効にする ONNX 意思決定エンジン                                |

## ロードマップ

- **Laya/ONNX 意思決定エンジン**：`morpheus-onnx` crate が提供する任意の `onnx` feature。[crate ガイド](../onnx/onnx.md)を参照してください。
- **グラフによる harness オーケストレーション**：複数の harness を協調させる新しいセッションモード。

## ライセンス

GPL-3.0。[LICENSE](../../LICENSE) を参照してください。
