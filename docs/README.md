# Documentação do Arandu

Esta pasta tem uma única fila de planejamento: o [roadmap mestre](arandu-compiler-roadmap-v0.1.md). Os demais documentos são decisões aceitas, contratos estáveis ou evidências; não crie checklists paralelos.

## Planejamento e decisões

- [Roadmap mestre](arandu-compiler-roadmap-v0.1.md) — fases abertas e decisões Gold consolidadas.
- [Projeto/pacotes Gold](arandu-project-package-lifecycle-gold-v0.1.md) — contrato detalhado implementado.
- [Guia de migração](arandu-project-package-migration-v0.1.md) — uso do contrato de projeto e dependências.
- [GenRef Gold](arandu-genref-gold-rfc-v0.1.md) — RFC aceita e implementada.
- [Nomes de anotações](arandu-attribute-naming-v0.1.md) — decisão PascalCase e migração.

## Contratos de arquitetura

| Área | Documentos |
| --- | --- |
| Frontend | [lexer](arandu-lexer-v0.1.md), [parser](arandu-parser-v0.1.md), [AST](arandu-ast-v0.1.md) |
| IR e execução | [AHIR](arandu-ahir-v0.1.md), [AMIR](arandu-amir-v0.1.md), [IR/SSA](arandu-ir-architecture-v0.1.md), [backends](arandu-backend-contract-v0.1.md) |
| ABI e memória | [ABI/layout](arandu-abi-layout-v0.1.md), [JIT/memória](arandu-jit-memory-v0.1.md), [stdlib](arandu-stdlib-architecture-v0.1.md) |
| Incrementalidade e IDE | [Salsa/LSP](arandu-salsa-lsp-architecture-v0.1.md), [LSP/editor](arandu-lsp-capabilities-v0.1.md), [CLI/LSP](arandu-cli-lsp-contract-v0.1.md) |
| Runtime e distribuição | [async runtime](arandu-async-runtime-design-v0.1.md), [instrumentação](arandu-compiler-instrumentation-v0.1.md), [distribuição](arandu-distribution-contract-v0.1.md) |

## Diagnósticos e releases

- [Especificação de diagnósticos](diagnostics/SPEC.md) e [catálogo por código](errors/).
- [Notas de release](releases/) são evidências imutáveis das versões publicadas.
- [OSSÀ virtual anchoring](ossa-virtual-anchoring.md) registra a decisão de estabilidade de IDs.

## Regras de manutenção

1. Uma decisão tem uma única fonte normativa.
2. Pesquisa concluída vira decisão, risco ou teste; notas temporárias são removidas.
3. Plano concluído é resumido no roadmap mestre e deixa de ser fila de trabalho.
4. Contratos declaram status, escopo e invariantes; exemplos longos ficam nos testes.
5. Antes do merge, valide links relativos e execute `xtask check-diag-docs`.
