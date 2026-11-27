# Morpheus

[English](../../README.md) | [Português](./README.pt-BR.md) | **Español** | [Русский](./README.ru.md) | [中文](./README.zh-CN.md) | [日本語](./README.ja.md)

<img width="1910" height="1064" alt="Interfaz de terminal de Morpheus" src="https://github.com/user-attachments/assets/a80fcff1-068a-44f8-b43d-3e7d7697ecd6" />

Morpheus es un coding agent de propósito general para la terminal. Incluye herramientas de ingeniería de software y *computer tools* que le permiten interactuar con aplicaciones gráficas.

## ¿Por qué Morpheus?

Morpheus no pretende reemplazar a los coding agents consolidados. Está diseñado para trabajar junto a ellos como una capa de orquestación. Estas son algunas razones para usarlo:

- **Colaboración multi-agente**: ejecuta distintos agentes de última generación (Claude, Gemini, Codex, Cline, etc.) en la misma TUI, creando un espacio de trabajo compartido donde pueden comunicarse a través de una única interfaz.
- **Ideal para experimentar**: perfecto para desarrolladores a los que les gusta probar agentes diferentes y combinar sus fortalezas en lugar de comprometerse con un único ecosistema.
- **Modo gestor**: sigue usando tus harnesses de agentes existentes mientras añades orquestación encima, especialmente útil cuando trabajas en varias tareas pequeñas a la vez.
- **Experiencia TUI nativa**: configura providers, modelos y modos de operación directamente en la interfaz con una configuración manual mínima.
- **Arquitectura modular**: crates independientes y reutilizables para herramientas de desarrollo, componentes de TUI, SDK y recall de memoria.

## En qué se diferencia de otros orquestadores TUI

| Recurso                                   | Morpheus                                                                                                                     | Otros orquestadores TUI                                                          |
| ----------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------- |
| **Paneles divididos multi-agente**        | Ejecuta varios agentes externos a la vez en paneles divididos, cada uno con su propia sesión, modelo y git worktree opcional | Suelen enfocarse en un solo agente o requieren cambiar de contexto               |
| **Computer tools vía accessibility tree** | Lee y opera interfaces gráficas mediante una representación estructurada, recurriendo a capturas solo cuando es necesario    | Normalmente dependen de capturas de pantalla o carecen de interacción con la GUI |
| **TUI basada en Rust**                    | Construida desde cero con Ratatui, inspirada en la UX de OpenCode                                                            | A menudo terminales web o interfaces menos optimizadas                           |
| **Configuración en la app**               | Providers, modelos y modos configurados directamente en la TUI                                                               | Suelen requerir archivos de configuración externos o flags de CLI                |
| **Crates modulares**                      | Herramientas de desarrollo, widgets de TUI, SDK y recall de memoria como bibliotecas independientes y reutilizables          | Arquitectura monolítica con componentes fuertemente acoplados                    |
| **Espacio de trabajo compartido**         | Varios agentes colaboran a través de una única interfaz con despacho coordinado de tareas                                    | Los agentes normalmente trabajan aislados                                        |

## Aspectos destacados

- **Modo multi-agente (`/agents`)**: ejecuta agentes externos (Gemini, Claude, Codex, Cline, …) junto con el agente principal en paneles divididos, todos alcanzables desde el mismo prompt. Cada agente mantiene su propia sesión, modelo y git worktree opcional, y el agente principal los coordina, despachando tareas y recopilando informes como un equipo.
- **Computer tools vía accessibility tree**: el agente lee y opera interfaces gráficas mediante una representación estructurada y compacta, y solo recurre a capturas de pantalla como respaldo. Menos imágenes procesadas, menos tokens consumidos.
- **TUI construida desde cero en Rust** (Ratatui), inspirada en la UX de OpenCode. La configuración de providers, modelos y modos de operación se hace directamente en la interfaz, con casi cero configuración manual.
- **Crates independientes**: las herramientas de desarrollo viven en crates desacoplados de la aplicación y pueden reutilizarse como bibliotecas fuera de Morpheus.

## Instalación

Probado en Linux y Windows. macOS aún no ha sido probado; si algo falla, [abre un issue](https://github.com/aidreamy/morpheus/issues) con el mayor detalle posible.

**Linux y macOS** (también funciona en Windows con Git Bash / WSL):

```sh
curl -fsSL https://raw.githubusercontent.com/aidreamy/morpheus/main/download.sh | bash
```

**Windows (PowerShell):**

```powershell
irm https://raw.githubusercontent.com/aidreamy/morpheus/main/download.ps1 | iex
```

> Nota: el comando `curl | bash` **no** funciona en PowerShell ni CMD, porque Windows no tiene `bash` nativo. Usa el script de PowerShell de arriba; Git Bash y WSL también funcionan.

Variables opcionales: `MORPHEUS_BIN_DIR` (directorio de instalación), `MORPHEUS_VERSION` (versión específica) y `MORPHEUS_VARIANT` (`slim`, el predeterminado, o `slim-embed`, con RAG local vía fastembed). Las compilaciones `slim-embed` se publican actualmente solo para Linux x64 y Windows x64; en macOS se distribuye `slim`. En PowerShell, defínelas como `$env:MORPHEUS_VERSION = "v0.1.0"` antes de ejecutar el comando.

Después de instalar, ejecuta `morpheus` en la terminal. Si el directorio de instalación no está en tu `PATH`, el propio script muestra el comando para agregarlo. Ante cualquier dificultad en la instalación, abre un issue y con gusto te ayudaremos.

## Crates

| Crate                                                           | Descripción                                                                                                            |
| --------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| [`morpheus-agents`](../../crates/morpheus-agents/README.md)     | Integración de subagentes: harnesses del cliente ACP, estado de sesión y streams de eventos tipados                    |
| [`morpheus-tools`](../../crates/morpheus-tools/README.md)       | Herramientas de desarrollo y computer tools                                                                            |
| [`morpheus-tui`](../../crates/morpheus-tui/README.md)           | Biblioteca de widgets de TUI: renderizado de markdown/diff y layout, los componentes detrás de la interfaz de terminal |
| [`morpheus-sdk`](../../crates/morpheus-sdk/README.md)           | Primitivas reutilizables para agentes: motor de parches, análisis de tool calls, cliente LSP e inteligencia AST        |
| [`morpheus-recall`](../../crates/morpheus-recall/README.md)     | Memoria y recall de contexto                                                                                           |
| [`morpheus-whatsapp`](../../crates/morpheus-whatsapp/README.md) | Integración con WhatsApp Web (multidispositivo): emparejamiento, eventos normalizados y envíos tipados                 |
| [`morpheus-onnx`](../../crates/morpheus-onnx/README.md)         | Motor de decisiones ONNX, habilitado por la feature `onnx`                                                             |

## Roadmap

- **Motor de decisiones Laya/ONNX**: feature opcional `onnx`, proporcionada por el crate `morpheus-onnx`; consulta la [guía del crate](../onnx/onnx.md).
- **Orquestación de harnesses en grafo**: un nuevo modo de sesión para coordinar múltiples harnesses.

## Licencia

GPL-3.0. Consulta [LICENSE](../../LICENSE).
