# Morpheus

[English](../../README.md) | **Português** | [Español](./README.es.md) | [Русский](./README.ru.md) | [中文](./README.zh-CN.md) | [日本語](./README.ja.md)

<img width="1910" height="1064" alt="Interface de terminal do Morpheus" src="https://github.com/user-attachments/assets/a80fcff1-068a-44f8-b43d-3e7d7697ecd6" />

Morpheus é um coding agent de propósito geral para o terminal. Ele inclui ferramentas de engenharia de software e *computer tools* que permitem interagir com aplicações gráficas.

## Por que Morpheus?

Morpheus não pretende substituir os coding agents consolidados. Ele foi projetado para trabalhar ao lado deles como uma camada de orquestração. Veja por que você pode querer usá-lo:

- **Colaboração multi-agente**: rode diferentes agentes de ponta (Claude, Gemini, Codex, Cline, etc.) na mesma TUI, criando um workspace compartilhado onde eles podem se comunicar por uma única interface.
- **Amigável à experimentação**: perfeito para desenvolvedores que gostam de testar agentes diferentes e combinar seus pontos fortes em vez de se prender a um único ecossistema.
- **Modo gerente**: continue usando seus harnesses de agentes existentes enquanto adiciona orquestração por cima, especialmente útil ao trabalhar em várias tarefas menores ao mesmo tempo.
- **Experiência TUI nativa**: configure providers, modelos e modos de operação diretamente na interface, com mínima configuração manual.
- **Arquitetura modular**: crates independentes e reutilizáveis para ferramentas de desenvolvimento, componentes de TUI, SDK e recall de memória.

## Como se diferencia de outros orquestradores TUI

| Recurso                                   | Morpheus                                                                                                                         | Outros orquestradores TUI                                            |
| ----------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------- |
| **Painéis divididos multi-agente**        | Rode vários agentes externos ao mesmo tempo em painéis divididos, cada um com sua própria sessão, modelo e git worktree opcional | Geralmente focados em um único agente ou exigem alternar de contexto |
| **Computer tools via accessibility tree** | Lê e opera interfaces gráficas por meio de uma representação estruturada, recorrendo a screenshots só quando necessário          | Costumam depender de screenshots ou não têm interação com GUI        |
| **TUI em Rust**                           | Construída do zero com Ratatui, inspirada na UX do OpenCode                                                                      | Muitas vezes terminais web ou interfaces menos otimizadas            |
| **Configuração dentro do app**            | Providers, modelos e modos configurados direto na TUI                                                                            | Costumam exigir arquivos de configuração externos ou flags de CLI    |
| **Crates modulares**                      | Ferramentas de desenvolvimento, widgets de TUI, SDK e recall de memória como bibliotecas independentes e reutilizáveis           | Arquitetura monolítica com componentes fortemente acoplados          |
| **Workspace compartilhado**               | Vários agentes colaboram por uma única interface com despacho coordenado de tarefas                                              | Agentes normalmente trabalham isolados                               |

## Destaques

- **Modo multi-agente (`/agents`)**: rode agentes externos (Gemini, Claude, Codex, Cline, …) junto com o agente principal em painéis divididos, todos alcançáveis pelo mesmo prompt. Cada agente mantém sua própria sessão, modelo e git worktree opcional, e o agente principal os coordena, despachando tarefas e coletando relatórios como um time.
- **Computer tools via accessibility tree**: o agente lê e opera interfaces gráficas por uma representação estruturada e compacta, e só recorre a screenshots como fallback. Menos imagens processadas, menos tokens consumidos.
- **TUI construída do zero em Rust** (Ratatui), inspirada na UX do OpenCode. Configuração de providers, modelos e modos de operação direto na interface, com quase zero configuração manual.
- **Crates independentes**: as ferramentas de desenvolvimento vivem em crates desacoplados da aplicação e podem ser reutilizadas como bibliotecas fora do Morpheus.

## Instalação

Testado em Linux e Windows. macOS ainda não foi testado; se algo falhar, [abra uma issue](https://github.com/aidreamy/morpheus/issues) com o máximo de detalhes.

**Linux e macOS** (também funciona no Windows via Git Bash / WSL):

```sh
curl -fsSL https://raw.githubusercontent.com/aidreamy/morpheus/main/download.sh | bash
```

**Windows (PowerShell):**

```powershell
irm https://raw.githubusercontent.com/aidreamy/morpheus/main/download.ps1 | iex
```

> Nota: o comando `curl | bash` **não** funciona no PowerShell ou CMD, porque o Windows não tem `bash` nativo. Use o script PowerShell acima; Git Bash e WSL também funcionam.

Variáveis opcionais: `MORPHEUS_BIN_DIR` (diretório de instalação), `MORPHEUS_VERSION` (versão específica) e `MORPHEUS_VARIANT` (`slim`, o padrão, ou `slim-embed`, com RAG local via fastembed). Builds `slim-embed` são publicados atualmente apenas para Linux x64 e Windows x64; no macOS é distribuído o `slim`. No PowerShell, defina-as como `$env:MORPHEUS_VERSION = "v0.1.0"` antes de executar o comando.

Depois de instalar, rode `morpheus` no terminal. Se o diretório de instalação não estiver no seu `PATH`, o próprio script mostra o comando para adicioná-lo. Qualquer dificuldade na instalação, abra uma issue, e teremos prazer em ajudar.

## Crates

| Crate                                                           | Descrição                                                                                                              |
| --------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| [`morpheus-agents`](../../crates/morpheus-agents/README.md)     | Integração de subagentes: harnesses do cliente ACP, estado de sessão e streams de eventos tipados                      |
| [`morpheus-tools`](../../crates/morpheus-tools/README.md)       | Ferramentas de desenvolvimento e computer tools                                                                        |
| [`morpheus-tui`](../../crates/morpheus-tui/README.md)           | Biblioteca de widgets de TUI: renderização de markdown/diff e layout, os componentes por trás da interface de terminal |
| [`morpheus-sdk`](../../crates/morpheus-sdk/README.md)           | Primitivas reutilizáveis para agentes: motor de patches, parsing de tool calls, cliente LSP e inteligência AST         |
| [`morpheus-recall`](../../crates/morpheus-recall/README.md)     | Memória e recall de contexto                                                                                           |
| [`morpheus-whatsapp`](../../crates/morpheus-whatsapp/README.md) | Integração com o WhatsApp Web (multi-device): pareamento, eventos normalizados e envios tipados                        |
| [`morpheus-onnx`](../../crates/morpheus-onnx/README.md)         | Motor de decisões ONNX, habilitado pela feature `onnx`                                                                 |

## Roadmap

- **Motor de decisões Laya/ONNX**: feature opcional `onnx`, fornecida pelo crate `morpheus-onnx`; veja o [guia do crate](../onnx/onnx.md).
- **Orquestração de harness em grafo**: um novo modo de sessão para coordenar múltiplos harnesses.

## Licença

GPL-3.0. Veja [LICENSE](../../LICENSE).
