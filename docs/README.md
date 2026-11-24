# Project documentation

This directory documents Morpheus's public crates and user-facing behavior.

## Documentation map

| Area         | Directory                                                         | Scope                                                                                                        |
| ------------ | ----------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| Entry points | [`usage`](usage/README.md) and [`readme`](readme/README.pt-BR.md) | Installing and using Morpheus                                                                                |
| SDK          | [`sdk`](sdk)                                                      | Patching, rollback, tool-call extraction, search, parsing and LSP                                            |
| Tools        | [`tools`](tools)                                                  | Filesystem, shell, search, web, desktop, skills, plans, questions and subagents                              |
| TUI          | [`tui`](tui)                                                      | Core renderables, layout, terminal utilities and the Solid layer                                             |
| ONNX         | [`onnx`](onnx/onnx.md)                                            | Decision models, routing, runtime, hooks, Hub integrity and language detection                               |
| Voice        | [`voice`](voice/README.md)                                        | Local text-to-speech: bundles, speakers, language frontends and `setup.json`                                 |
| Telemetry    | [`telemetry`](telemetry/README.md)                                | Collected metrics, local storage, transport and privacy boundaries                                           |
