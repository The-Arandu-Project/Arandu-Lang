# SL_T.4 — Benchmark engine

**Estado:** implementado; promoção Gold depende da matriz nativa do PR  
**Pré-requisito:** SL_T.3 concluído  
**Objetivo:** medir microbenchmarks Arandu com uma região temporizada explícita,
amostragem auditável e proteção contra eliminação do workload nos dois backends,
sem transformar ruído de uma máquina em promessa de desempenho.

## Evidência externa confrontada

- O [`testing.B` do Go](https://pkg.go.dev/testing#B) calibra o número de
  iterações até obter duração suficiente e recomenda `B.Loop` para impedir que
  setup seja repetido ou entre acidentalmente na medição. Arandu adota um loop
  controlado pelo harness, mas não convenções de nome nem estado global.
- O [processo de análise do Criterion.rs](https://bheisler.github.io/criterion.rs/book/analysis.html)
  separa warmup, medição, análise e comparação e mede lotes completos, não cada
  operação isolada. Arandu adota warmup, calibração e amostras em lotes; a
  comparação estatística permanece em SL_T.5.
- Os [timing loops do Criterion.rs](https://bheisler.github.io/criterion.rs/book/user_guide/timing_loops.html)
  mostram que setup por iteração pode introduzir várias ordens de grandeza de
  overhead e que lotes excessivos podem esgotar memória. Arandu terá políticas
  limitadas e explícitas, sem escolher um lote ilimitado a partir da velocidade.
- O [Google Benchmark](https://google.github.io/benchmark/user_guide.html)
  demonstra warmup, tempo mínimo, repetições, JSON e barreiras de otimização,
  mas também alerta que `DoNotOptimize(expr)` não impede toda otimização interna
  de `expr`. `blackBox` será identidade best-effort na IR e nunca garantia de
  segurança ou de execução constante.
- A documentação de [`std::hint::black_box`](https://doc.rust-lang.org/stable/std/hint/fn.black_box.html)
  explicita que a barreira é dependente de plataforma/backend e não serve para
  correção ou criptografia. O contrato público do Arandu terá o mesmo limite.
- As [recomendações do LLVM](https://llvm.org/docs/Benchmarking.html) distinguem
  redução de ruído de eliminação de viés. Por isso o runner registra ambiente e
  amostras, mas não declara máquinas diferentes comparáveis nem põe tempo
  absoluto de runner compartilhado no `S0 / Gate`.

## Decisões consolidadas

1. **A medição acontece dentro do processo do harness.** Spawn, descoberta,
   compilação, JIT, setup anterior ao primeiro `loop()` e cleanup posterior não
   entram na janela. A CLI apenas coordena e apresenta resultados.
2. **Um sample mede um lote.** O relógio não envolve cada operação rápida. O
   tempo do lote é dividido por sua contagem usando aritmética verificada.
   Overhead de loop/relógio não é subtraído: essa correção pode produzir
   resultados negativos ou amplificar erro.
3. **Warmup e calibração são descartados.** Uma fase piloto cresce a contagem
   geometricamente até atingir resolução útil; limites de iterações, duração e
   bytes impedem overflow, hang e alocação sem limite.
4. **Amostras brutas são a evidência.** O formato conserva duração e iterações
   de cada sample. A apresentação v0.1 deriva mediana, MAD, p50/p95, mínimo e
   dispersão; não mascara outliers nem afirma regressão estatística em SL_T.4.
5. **`blackBox` pertence ao compilador.** A stdlib expõe a função, AMIR modela a
   operação explicitamente, visitors/dataflow/DCE a reconhecem e C/Cranelift
   baixam para uma barreira ABI equivalente. Uma chamada Arandu comum não é
   prova contra inlining, propagação constante ou DCE.
6. **Setup e cleanup não são mágicos.** O contrato inicial mede somente o corpo
   delimitado por `while bench.loop()`. APIs batched posteriores devem declarar
   se setup é por lote ou por iteração; não haverá `pause/resume` arbitrário no
   hot loop, fonte frequente de erro e overhead.
7. **Benchmarks são serializados por padrão.** `--jobs` não será herdado
   implicitamente de `arandu test`: concorrência entre benchmarks altera cache,
   frequência e contenção. Paralelismo futuro precisa ser opt-in e registrado.
8. **O protocolo é irmão, não sobrecarga do protocolo de testes.** Reutilizamos
   framing, limites e isolamento de SL_T.2, mas resultados usam schema
   `arandu.bench/v1`; nenhum `TestEventV1` ganhará campos opcionais ambíguos.

## Contrato público v0.1

```arandu
import std.testing as testing

@Benchmark
func parseSmallFile(mut bench: testing.Benchmark): void {
    input = loadFixture()
    while bench.loop() {
        testing.blackBox(parse(input))
    }
}
```

- Alvo válido: função livre, síncrona, não genérica, um parâmetro
  `mut testing.Benchmark` e retorno `void`.
- `bench.loop()` é a única autoridade sobre warmup, calibração e samples; código
  do usuário não recebe a contagem interna como fonte de identidade.
- `testing.blackBox(value)` devolve o mesmo valor e aceita apenas os tipos que
  possuam lowering equivalente comprovado nos dois backends. A superfície pode
  começar por escalares, referências/strings e crescer sem enfraquecer o ABI.
- CLI mínima: `arandu bench [filtro]`, `--exact`, `--list`, `--warmup`,
  `--measurement-time`, `--samples`, `--format human|json` e `--output`.
- Defaults são parte da versão do schema e aparecem na saída efetiva; valores
  inválidos ou que excedam caps são erro de uso, não clamp silencioso.

## Fases de implementação

### SL_T.4A — Descoberta, contrato e artefato — concluído

- Promover `@Benchmark` de reservada para implementada e validar a assinatura
  no proprietário semântico, com diagnóstico documentado e span real.
- Criar caso/manifesto incremental por item, separado do manifesto de testes,
  preservando IDs canônicos e early-cutoff de irmãos/importadores.
- Estender o modo de build do harness com registry e entrypoint de benchmark
  ordenados, usando o mesmo grafo, lockfile, perfil e backend do projeto.
- Implementar `arandu bench --list`, filtros e execução exata sem reparse ou
  varredura paralela de arquivos.

**Gate:** assinatura inválida é rejeitada deterministicamente; edição apenas no
corpo não muda o manifesto; C e Cranelift listam os mesmos IDs.

### SL_T.4B — Relógio, máquina de estados e calibração — concluído

- Modelar `Benchmark` como máquina de estados finita: `created →
  warmup/calibrating → measuring(samples) → finished`. Chamadas extras a `loop()` não
  reiniciam o relógio nem produzem outro resultado terminal.
- Usar o relógio steady já exposto pelo runtime e validar monotonicidade,
  resolução e conversões. Falha do relógio vira evento operacional estruturado.
- Calibrar com crescimento geométrico saturado e teto de iterações/tempo. Para
  workload abaixo da resolução, aumentar o lote em vez de publicar `0 ns/op`.
- Executar warmup descartado antes dos samples e manter preparação do processo,
  JIT e registry fora da janela.

**Gate:** relógio de teste injetável cobre avanço zero, baixa resolução,
overflow, duração excessiva e transições ilegais sem loop infinito ou panic.

### SL_T.4C — Batching e barreira de otimização — concluído

- Introduzir a menor variante AMIR capaz de representar `blackBox` como
  identidade observável para otimização, atualizando visitors compartilhados,
  hashing, DCE, liveness, move checking e impressão de IR de forma exaustiva.
- Baixar a operação por uma ABI de runtime tipada e `noinline`/opaca onde
  necessário, equivalente em C e Cranelift. Não depender de inline assembly
  exclusivo de uma arquitetura nem de `volatile` como substituto universal.
- Fornecer batch automático limitado para workloads pequenos. Setup mutável por
  iteração fica fora da primeira API até existir uma forma explícita que não
  acumule outputs nem esconda custo de drop.
- Provar que input e output protegidos não são propagados/eliminados e que o
  comportamento fora do modo de benchmark continua inalterado.

**Gate:** fixtures AMIR antes/depois de DCE conservam o workload; executáveis C
e Cranelift produzem o mesmo resultado e chamam a barreira na quantidade
esperada. Sanitizers/Miri cobrem a ABI quando aplicável.

### SL_T.4D — Protocolo, estatística e reporters — concluído

- Definir DTOs estreitos para configuração efetiva, metadados do ambiente,
  samples `{iterations, elapsed_ns}`, métricas derivadas e falha operacional.
- Preservar inteiros de tempo/contagem no JSON; valores por operação derivados
  usam representação definida, sem depender de locale ou `f64::Debug`.
- Calcular mediana, MAD, p50/p95 e mínimo sobre os samples válidos. Média e
  desvio podem ser informativos, mas nunca a única síntese.
- Registrar versão Arandu, alvo, backend, perfil, SO/arquitetura, fonte do
  relógio, warmup e parâmetros. Não registrar path absoluto, hostname ou
  identificador pessoal como parte reproduzível.
- Reporter humano mostra unidade adaptativa e alerta sobre amostra insuficiente
  ou ruído alto; JSON mantém ordem canônica e todas as amostras.

**Gate:** round-trip do `arandu.bench/v1`, snapshots independentes de locale e
CRLF/LF, rejeição de NaN/infinito/frame excessivo e saída atômica em arquivo.

### SL_T.4E — Campanha adversarial e promoção Gold — implementado localmente

- Cobrir workload vazio, constante, puro com resultado ignorado, mutação,
  alocação, I/O acidental, duração abaixo da resolução, sample muito lento,
  crash, timeout, stdout hostil, Unicode e nenhuma correspondência.
- Provar que setup/cleanup não entra no contador e que cleanup ainda ocorre em
  sucesso, falha operacional e cancelamento recuperável.
- Rodar os artefatos públicos fora do checkout em Windows x86-64, Linux x86-64
  e macOS ARM64, nos backends suportados pelo contrato de distribuição.
- Manter apenas testes funcionais determinísticos no gate obrigatório. Números
  absolutos e thresholds de performance rodam em campanha informativa até
  SL_T.5 fornecer baseline e ambiente comparável.

**Gate final:** nenhuma otimização suportada apaga o workload protegido; cada
resultado contém múltiplas amostras auditáveis; falhas não viram números; SDK
público executa `arandu bench` sem Rust, Cargo, Python ou checkout.

**Evidência local:** descoberta e cutoff por item, contrato `T037`, máquina de
estados com relógio injetável, DCE, paridade C/Cranelift da barreira, framing,
estatística robusta e E2E de processo passam no Windows. A promoção para Gold
continua condicionada aos mesmos artefatos públicos nos runners nativos Linux
x86-64 e macOS ARM64; resultado local não antecipa suporte multiplataforma.

## Riscos conhecidos e mitigação

| Risco observado em implementações maduras | Decisão Arandu |
| --- | --- |
| Medir uma única execução ou publicar apenas média | múltiplos samples brutos + estatística robusta |
| Subtrair overhead estimado e criar tempo negativo | batching/calibração; nenhuma subtração automática |
| Setup/drop contaminar o hot loop | região de `loop()` e políticas explícitas |
| `blackBox` comum ser inlined ou apagado | operação AMIR + ABI comprovada nos dois backends |
| Batch crescer até consumir memória | caps de contagem, duração e bytes |
| CI compartilhado acusar regressão falsa | SL_T.4 mede; SL_T.5 compara com ambiente/limiar explícitos |
| Concorrência aquecer/estrangular workloads de modo desigual | serial por padrão e metadados efetivos |
| JSON derivar do texto humano | DTO/schema versionado independente do console |

## Validação obrigatória

Cada subfase executa a sequência integral do `AGENTS.md`. Mudanças em query
também rodam os testes de cutoff/arquitetura relevantes; mudanças AMIR rodam
fixtures de DCE, CFG, move/liveness e ambos os backends; mudanças de protocolo
ganham E2E de processo nos três sistemas. Snapshots nunca fixam duração real.

## Fora do SL_T.4

- comparação A/B, persistência de baseline e gate por regressão (`SL_T.5`);
- JUnit, CodeLens e apresentação completa no VS Code (`SL_T.5`);
- contagem de alocações antes de haver instrumentação equivalente entre
  runtimes/backends;
- benchmarks async antes de SL_R Gold;
- perf counters, afinidade/priority automáticas, benchmark distribuído e
  comparação entre máquinas não equivalentes;
- garantia criptográfica ou de constant-time para `blackBox`.
