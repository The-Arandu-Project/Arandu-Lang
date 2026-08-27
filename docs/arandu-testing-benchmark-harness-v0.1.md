# Arandu — Testing & Benchmark Harness v0.1

**Status:** campanha ativa (`SL_T`)  
**Roadmap executivo:** [Arandu Compiler Architecture](./arandu-compiler-roadmap-v0.1.md)  
**Escopo:** testes e microbenchmarks de programas Arandu; a infraestrutura de
testes do próprio compilador continua em `cargo test`, `xtask` e nos gates de CI.

Este documento é um contrato técnico de implementação, não um roadmap
concorrente. O estado executivo de `SL_T` permanece no roadmap principal.

## Resultado Gold

Ao concluir `SL_T`, um projeto Arandu poderá descobrir, compilar e executar
testes e benchmarks com os mesmos artefatos, resolução de pacotes e pipeline
usados por `arandu check/build/run`. A saída será útil para pessoas e estável
para ferramentas, sem reflexão em runtime, parsing de texto humano ou
dependência de Python, Cargo e `xtask` na instalação pública.

```text
fonte + @Test/@Benchmark
        │
        ▼
queries canônicas (CST → AST → resolve → typeck → AMIR)
        │
        ▼
manifesto determinístico de casos + executável de harness
        │
        ├── protocolo estruturado versionado ──► CLI/reporters
        └── stdout/stderr do caso, isolados dos eventos do runner
```

## Evidência externa e decisões

- O [pacote `testing` do Go](https://pkg.go.dev/testing) demonstra descoberta
  integrada, filtros e nomes hierárquicos, cleanup, isolamento de saída e um
  loop de benchmark que exclui setup/teardown e impede eliminação indevida.
  Arandu adota esses contratos, mas não a convenção mágica `TestXxx`.
- O [Swift Testing](https://developer.apple.com/documentation/testing) usa
  `@Test`, testes parametrizados e expectativas que preservam os valores
  avaliados. Arandu adota descoberta explícita e falhas estruturadas; testes
  parametrizados ficam depois do runner mínimo.
- O [`zig test`](https://ziglang.org/documentation/master/#Zig-Test) gera um
  executável com runner integrado. Arandu segue essa separação entre build e
  execução, sem criar uma segunda rota de parser ou backend.
- O [runner de testes do Rust](https://doc.rust-lang.org/book/ch11-02-running-tests.html)
  evidencia a velocidade do paralelismo, mas também a interferência por estado
  global. Arandu começa determinístico e serial; paralelismo é controlado por
  `--jobs` e só ocorre entre processos isolados.
- O benchmark embutido do Rust permanece uma
  [API instável](https://doc.rust-lang.org/unstable-book/library-features/test.html).
  Arandu não expõe internals do compilador como API pública do harness.
- O [Google Benchmark](https://google.github.io/benchmark/user_guide.html)
  mostra a importância de warmup, calibração, repetições, metadados e formatos
  estruturados. As [recomendações do LLVM](https://llvm.org/docs/Benchmarking.html)
  reforçam que repetição reduz ruído, mas não elimina viés. Por isso a Gold não
  transforma uma única medição de CI em gate de regressão.

## Decisões de arquitetura

### Propriedade por camada

| Área | Responsabilidade |
| --- | --- |
| `arandu_parser` | Preservar `@Test`/`@Benchmark` no CST/AST existente; nenhuma descoberta paralela. |
| `arandu_semantics` | Registro canônico PascalCase, validação de alvo, argumentos e assinatura. |
| `arandu_query` | Queries incrementais por item para casos validados e manifesto hash-estável. |
| `arandu_codegen` + backends | Gerar o entrypoint do harness pelo pipeline normal e preservar a barreira de otimização do benchmark. |
| `arandu_runtime` | ABI mínima de eventos, captura, tempo monotônico e término controlado; sem política de CLI. |
| `stdlib/testing` | Expectativas, contexto, cleanup, skip, temporários e API do loop de benchmark. |
| `arandu_cli` | `arandu test`/`arandu bench`, seleção de pacotes, build, processos, timeouts e reporters. |
| `xtask` | Apenas valida o próprio repositório Arandu; nunca é dependência do usuário. |

Não será criado um crate “dono de tudo”. DTOs compartilhados devem ficar no
crate mais estreito que possa expressá-los sem levar Salsa, CLI ou filesystem
para as fases puras.

### Descoberta e identidade

- `@Test` e `@Benchmark` são nomes canônicos. Não haverá descoberta por prefixo
  do nome da função nem registro global mutável por inicializadores.
- A primeira versão aceita funções livres, sem parâmetros e síncronas. Testes
  retornam `void` ou `Result<void, E>`; benchmarks recebem um contexto do
  módulo `testing` e retornam `void`.
- Métodos, async, casos parametrizados e fixtures de suite ficam reservados até
  haver contrato de receiver/runtime que não introduza comportamento mágico.
- O ID público é derivado de pacote, alvo, módulo e símbolo canônicos. Spans,
  offsets, `FileId` e ordem de `HashMap` nunca participam da identidade.
- O manifesto é ordenado e gerado em compilação. Edições em um corpo que não
  alterem a lista ou a assinatura de casos não invalidam consumidores da
  superfície de descoberta.

### Processo e protocolo

- Cada executável de harness oferece listagem e execução por IDs exatos. A CLI
  agenda os casos e mantém a ordenação de apresentação estável.
- O transporte interno usa eventos estruturados enquadrados e versionados;
  stdout/stderr do programa não compartilham o canal de controle. Reporters
  nunca inferem sucesso ou falha analisando texto.
- Estados mínimos: `started`, `passed`, `failed`, `skipped`, `timed_out` e
  `crashed`, com duração, localização, falha estruturada e saída capturada.
- O processo retorna códigos distintos para sucesso, falha de testes, erro de
  uso e falha operacional/compilação, alinhados ao contrato tipado da CLI.
- Execução padrão é serial e reproduzível. `--jobs N` habilita processos
  paralelos isolados; mutações de cwd e ambiente nunca são concorrentes dentro
  do mesmo processo.
- Interrupção, timeout e crash não podem truncar silenciosamente o relatório.

### Experiência de teste

Superfície mínima planejada:

```arandu
import std.testing as testing

@Test
func sumTwoValues(): void {
    testing.expectEqual(4, 2 + 2)
}
```

- `expect`, `expectEqual` e `fail` produzem uma falha estruturada com expressão,
  valores avaliados e span original; não apenas uma string ou abort genérico.
- Cada operando é avaliado uma vez. A captura é lowering explícito do
  compilador, não macro textual nem reparse da expressão.
- `skip`, logs, cleanup LIFO e diretório temporário são operações do contexto
  do caso. Cleanup roda em sucesso, falha e retorno antecipado; crash de
  processo é reportado, mas não promete executar código do processo morto.
- Filtros suportam substring literal e `--exact`; regex não é requisito da
  primeira Gold. `--list`, `--fail-fast`, `--timeout`, `--jobs` e `--seed`
  compõem a CLI mínima.
- Saída humana é concisa e colorida apenas em terminal. `--format json` emite
  schema versionado; JUnit é um reporter posterior, não o protocolo interno.

### Experiência de benchmark

```arandu
import std.testing as testing

@Benchmark
func parseSmallFile(bench: mut testing.Benchmark): void {
    input = loadFixture()
    while bench.loop() {
        testing.blackBox(parse(input))
    }
}
```

- `bench.loop()` delimita exatamente a região medida: setup anterior e cleanup
  posterior não entram no cronômetro.
- O runner usa relógio monotônico, warmup descartado, calibração de iterações,
  batching e múltiplas amostras. Nunca publica “média de uma execução”.
- `blackBox`/keep-alive é uma barreira reconhecida na IR e coberta em ambos os
  backends; uma função comum de biblioteca não é garantia suficiente contra DCE.
- O resultado preserva amostras brutas e apresenta mediana, dispersão robusta,
  percentis, iterações e unidade. Média/desvio podem ser derivados, mas não são
  a única evidência.
- O schema inclui versão do Arandu, alvo, backend, perfil, SO/CPU, relógio,
  configuração, warmup e métricas customizadas. Contagem de alocações só é
  publicada quando houver instrumentação equivalente nos runtimes suportados.
- `arandu bench --compare <baseline>` informa diferença prática e incerteza.
  Falha automática exige limiar explícito e ambiente controlado; benchmarks
  ruidosos não entram no `S0 / Gate`.

## Plano de implementação

### SL_T.0 — Contratos e cortes incrementais

**Estado:** `done` no escopo de descoberta e listagem; o protocolo de eventos
entra com o primeiro executável de harness em `SL_T.1`.

- Promover `@Test` e registrar `@Benchmark` como anotações planejadas com
  assinaturas e diagnósticos definidos.
- Introduzir caso/ID/manifesto semântico determinístico por item e testes de
  early-cutoff, ciclos, Unicode e ordem de módulos.
- Fixar schema de eventos v1, códigos de saída e layout dos artefatos de teste.

**Saída:** `arandu test --list` pode usar o grafo real e listar casos válidos,
sem compilar um executável e sem varrer fontes por conta própria.

### SL_T.1 — Build e executável de harness

**Estado:** `done` para o runner host e artefatos de harness da versão v0.1.

- Criar modo de build de teste sobre o mesmo grafo, lockfile e queries do
  pacote, incluindo fontes de `tests/` como alvo de integração separado.
- Gerar registry/entrypoint ordenado em `arandu_codegen`, com shim C
  determinístico e execução host via Cranelift, sem exigir `main` escrito pelo
  usuário.
- Publicar manifesto e shim em `target/<profile>/<triple>` por escrita atômica,
  usando as mesmas políticas `--locked`, `--offline` e `--frozen` do projeto.

**Saída:** um harness público executa um caso exato fora do checkout.

### SL_T.2 — Runner confiável e reporters

- Implementar `arandu test` com list/filter/exact, captura, timeout, fail-fast,
  seed, jobs, interrupção e classificação de crash.
- Adicionar protocolo estruturado, reporter humano e JSON versionado.
- Testar stdout hostil, payload truncado, processo morto, nomes Unicode,
  timeouts e ordenação em Windows, Linux e macOS.

**Saída:** testes de projeto são usáveis em terminal e CI sem flakiness de
protocolo ou dependência do texto apresentado.

### SL_T.3 — `std.testing` e diagnósticos de expectativa

- Entregar contexto de caso, expect/expectEqual/fail/skip/log, cleanup LIFO e
  diretórios temporários com containment e remoção segura.
- Implementar lowering de expectativas com avaliação única, valores e spans.
- Acrescentar documentação de cada diagnóstico público e quick navigation no
  LSP; CodeLens de execução fica condicionado ao protocolo já estável.

**Saída:** falhas explicam expressão, esperado e encontrado no local correto.

### SL_T.4 — Benchmark engine

- Registrar `@Benchmark`, contexto e `loop`, relógio monotônico, warmup,
  calibração, batching, repetições e barreira IR `blackBox`.
- Provar que DCE não elimina o workload e que setup/cleanup não contaminam a
  medição nos dois backends.
- Produzir saída humana e JSON com amostras e metadados reproduzíveis.

**Saída:** `arandu bench` mede microbenchmarks honestos nos sistemas suportados.

### SL_T.5 — Comparação e integração de produto

- Adicionar baseline/compare, limiares explícitos, dry-run e exportação JUnit
  para testes; preservar formato de benchmark independente do console.
- Integrar comandos à extensão somente depois da CLI Gold, com descoberta em
  background e sem análise duplicada no editor.
- Documentar unitários versus integração, fixtures, isolamento e metodologia
  de benchmark.

**Saída:** fluxo local, CI e editor compartilham IDs e resultados canônicos.

### SL_T.6 — Campanha Gold

- E2E nativo de package → test/bench em Windows, Linux e macOS, fora do repo e
  usando o SDK público.
- Campanhas adversariais de protocolo, filesystem, concorrência, recovery,
  determinismo e ruído de benchmark.
- Gate específico `SL_T / Harness`; `S0 / Gate` recebe apenas provas rápidas e
  determinísticas. Zero P0/P1 aberto no escopo publicado.

**Saída:** marcar `SL_T` como `gold` no roadmap e reduzir este documento ao
contrato vivo, preservando evidências históricas no Git/CI.

## Fora da primeira Gold

- fuzzing de programas de usuário, property testing e geração automática de
  casos;
- coverage até existir instrumentação coerente entre backends;
- testes async até `SL_R` oferecer scheduler, cancelamento e relógio testável;
- benchmarks distribuídos, perf counters específicos de SO e comparação entre
  máquinas não equivalentes;
- plugins arbitrários de runner, scripts de dependência e execução remota.

Esses itens podem evoluir sem alterar o protocolo v1 nem enfraquecer a
reprodutibilidade do runner básico.

## Critérios de aceitação Gold

- Uma única rota CST-first e Salsa alimenta build normal, testes e benchmarks.
- Descoberta e saída são determinísticas e não usam offsets incidentais.
- Falhas, skips, timeout e crash são distintos e machine-readable.
- Estado global não torna o paralelismo implicitamente inseguro.
- Setup/cleanup não entram na medição; DCE não apaga o workload.
- Resultados preservam amostras e contexto suficiente para auditoria.
- SDK público executa o fluxo completo sem Rust, Cargo, Python ou checkout.
- Gates nativos provam o contrato nos três sistemas suportados.
