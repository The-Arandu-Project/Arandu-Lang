# Auditoria de Arquitetura e Performance v0.1

**Status:** auditoria estática concluída; guardrails implementados

## Visão Geral e Contexto

Esta auditoria pausa expansão funcional e verifica ownership entre crates,
efeitos, incrementalidade, portabilidade, concentração de módulos e sinais de
alocação. Ela não declara ganho de performance a partir de contagem de linhas
ou de `.clone()`: essas contagens apenas selecionam locais para inspeção.

O resultado geral é saudável. Frontend, semântica pura, query engine,
backends, runtime e frontends CLI/LSP têm ownership distinguível. Não foi
encontrado filesystem I/O em `src/` dos crates puros de compilação, nem
providers Salsa fora de `arandu_query`. A exceção declarativa existente em
`arandu_middle/src/db.rs` abriga tipos compartilhados da DB, sem executar
queries.

## Detalhes Técnicos da Implementação

### Fronteiras e dependências

| Camada | Dono | Resultado da auditoria |
| --- | --- | --- |
| texto/CST/AST | lexer e parser | CST-first preservado; nenhuma tipagem/resolução deslocada ao parser |
| contratos/IR | middle | HIR, AMIR, IDs e layout continuam independentes dos frontends |
| semântica | resolve, typeck, mir, semantics | fontes puras; nenhum acesso direto ao filesystem em produção |
| incremental | query | providers, host, snapshots e projeções continuam concentrados |
| emissão | codegen e backends | contratos de teste/ABI compartilhados sem dependência da CLI |
| efeitos | CLI, LSP, runtime e xtask | I/O, processos, cache, rede e publicação permanecem nas bordas |

Foi removida a dependência direta e sem uso de Salsa em `arandu_lsp`. O novo
`xtask check-architecture`, executado no S0, rejeita dependência/uso Salsa fora
do owner e do contrato estreito de `middle`, e rejeita I/O de filesystem nos
sources dos crates puros. `arandu_base/src/tracing_bridge.rs` é a exceção
explícita: somente o sink de self-profile grava o arquivo solicitado pela CLI.

### Tamanho e coesão

Os maiores arquivos de produção observados foram `arandu_cli/src/main.rs`
(~2,1 mil linhas), `arandu_lsp/src/ide.rs` (~1,8 mil),
`arandu_cli/src/test_runner.rs` (~1,55 mil após a extração), o JIT Cranelift (~1,6 mil), cache da
CLI (~1,4 mil) e manifest/query (~1,3 mil). Tamanho isolado não é defeito:
emitters, pretty-printers e tabelas de diagnóstico podem ser longos e coesos.

Os pontos de separação confirmados são por responsabilidade, não por quota de
linhas. `main.rs` mistura parsing de argumentos, pipeline e comandos de
projeto; `test_runner.rs` ainda mistura coordenação de processos, protocolo,
baseline e reporters JSON/humano, enquanto estatística e JUnit agora vivem em
submódulos puros; `ide.rs` agrega várias capacidades LSP. A
próxima alteração funcional em cada superfície deve extrair o respectivo
domínio com testes inalterados, sem uma reescrita transversal nesta auditoria.

### Heap, clones e strings

A inspeção encontrou maior concentração de `.clone()` na orquestração da
CLI, linking do HIR, monomorfização, grafo de pacotes e filas incrementais.
Nos hot paths query/IR, o projeto já usa `Arc`, interning, `SmolStr`,
`rustc_hash`, resultados por item e sharing Salsa. Nos frontends, muitas cópias
são ownership necessário para atravessar threads, processos ou DTOs JSON/LSP.

Nenhuma troca ampla por `Cow`, `SmallVec`, novos interners ou outro allocator
foi aplicada: isso alteraria complexidade/tamanho sem evidência. Cópias
obviamente mortas podem ser removidas em revisão local; estruturas de dados e
hot paths exigem workload representativo, perfil de alocação e benchmark antes
e depois.

### Documentação e portabilidade

A taxonomia agora distingue roadmap, contrato, arquitetura, decisão concluída,
diagnóstico e release. Quatro roadmaps SL_T concluídos foram removidos e seu
conteúdo foi consolidado no contrato de testes/benchmarks. Project/Package e
GenRef deixaram de parecer campanhas abertas. O contrato de texto UTF-8/LF foi
materializado em `.gitattributes`, `.editorconfig`, xtask e CI.

## PONTOS DE MELHORIA (O que não está no roadmap)

- Extrair parsing/despacho de project lifecycle de `arandu_cli/src/main.rs`
  quando a superfície receber a próxima mudança.
- Continuar separando processo/IPC, benchmark/baseline e reporters JSON/humano
  de `arandu_cli/src/test_runner.rs`; estatística e JUnit já foram extraídos e
  o protocolo permanece em `arandu_codegen`.
- Separar capacidades de apresentação em `arandu_lsp/src/ide.rs` por DTO
  compartilhado, sem duplicar type presentation ou criar novas queries.
- Renomear ou extrair futuramente a library `arandu_package`, hoje publicada
  pelo package Cargo `arandu_cli`, para que dependências do LSP não pareçam uma
  dependência do binário completo.
- Tornar o guardrail arquitetural menos lexical no futuro. O check atual é
  deliberadamente simples e pode evoluir para inspeção do grafo Cargo/AST.
- Medir parsing, edição incremental, typeck por item, build noop, LSP latency e
  compilação de projetos grandes antes de qualquer campanha de otimização.

## Futuro e Próximos Passos

1. Fechar o soak do harness SL_T e manter sua matriz Gold.
2. Definir um corpus de performance estável antes da primeira campanha de
   heap/latência; capturar tempo, recomputação Salsa, pico de RSS e alocações.
3. Fazer as extrações modulares acima junto da próxima mudança do domínio,
   limitando cada PR a uma responsabilidade e preservando testes Gold.
4. Manter o master roadmap como única fila e consolidar todo plano temporário
   assim que a campanha correspondente terminar.

### Validação de mercado

O modelo segue o red-green incremental do rustc: pureza, fingerprints estáveis
e projeções pequenas evitam propagação falsa. O rust-analyzer confirma a
necessidade de cancelamento/snapshots ao aplicar mudanças Salsa. O critério de
layering do LLVM reforça dependências explícitas e acíclicas entre bibliotecas.
Para heap, o Rust Performance Book recomenda que contagens de clone/alocação
levem a profiling, e não a substituições automáticas; `rustc-perf` reforça o
uso de corpus contínuo para atribuir regressões.

- [rustc incremental compilation](https://rustc-dev-guide.rust-lang.org/queries/incremental-compilation.html)
- [rust-analyzer architecture](https://rust-analyzer.github.io/book/contributing/architecture.html)
- [LLVM library layering](https://llvm.org/docs/CodingStandards.html#library-layering)
- [Rust Performance Book: heap allocations](https://nnethercote.github.io/perf-book/heap-allocations.html)
- [rustc-perf](https://github.com/rust-lang/rustc-perf)
