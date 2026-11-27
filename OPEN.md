# Ideias em aberto

## Despacho adaptativo de agentes

Fase futura, após a execução durável e a integração verificada de patches.

O `auto` atual escolhe um provedor pela cadeia de fallbacks, e o
`agents_call` delega a um painel escolhido pelo coordenador. A proposta é
escolher a estratégia de execução conforme as dependências do trabalho,
as ferramentas de cada agente e o orçamento disponível.

- Usar um agente em mudanças pequenas ou com etapas sequenciais.
- Distribuir frentes independentes entre os agentes que têm as ferramentas
  adequadas.
- Acionar uma segunda investigação quando evidências conflitantes impedirem
  a validação da primeira.

Limitar custo, tempo e concorrência por execução. Comparar as rotas com
uma execução de agente único usando custo por patch aceito, tempo até a
validação e taxa de retrabalho.

Referências

- [Google Research sobre escala de sistemas multiagente](https://research.google/blog/towards-a-science-of-scaling-agent-systems-when-and-why-agent-systems-work/)
- [WorkflowArena](https://arxiv.org/abs/2611.12856), preprint sobre
  orquestração executável.
