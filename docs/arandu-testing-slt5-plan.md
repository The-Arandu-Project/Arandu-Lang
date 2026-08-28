# SL_T.5 — Comparação e integração de produto

**Estado:** `done` na implementação local; promoção Gold depende de SL_T.6  
**Pré-requisito:** SL_T.4 implementado  
**Dono dos contratos:** CLI/reporters sobre `arandu.test/v1` e `arandu.bench/v1`

## Objetivo

Transformar os eventos canônicos do harness em um fluxo compartilhado entre
terminal, CI e VS Code. Descoberta e execução continuam pertencendo ao
compilador/CLI; consumidores não analisam fonte nem interpretam texto humano.

## Referências e decisões

- O [Criterion](https://docs.rs/criterion/latest/criterion/enum.Baseline.html)
  separa salvar, comparar de forma permissiva e comparar estritamente. Arandu
  adota esses estados, mas mantém seu engine e protocolo próprios.
- O processo do
  [Criterion](https://docs.rs/criterion/latest/criterion/struct.Criterion.html)
  separa warmup, medição, análise e comparação. SL_T.5 compara somente as
  amostras produzidas pelo engine SL_T.4.
- O GitHub recomenda publicar resultados de teste como
  [workflow artifacts](https://docs.github.com/en/actions/tutorials/store-and-share-data).
  O JUnit do Arandu é evidência portátil; JSON continua sendo o contrato rico.
- A [Testing API do VS Code](https://code.visualstudio.com/api/extension-guides/testing)
  oferece descoberta, execução, estados e output estruturados. A extensão usa
  essa API e não cria um segundo analisador.

Problemas evitados:

- uma única medição não decide regressão;
- runners ou CPUs incompatíveis não são comparados silenciosamente;
- baseline ausente é permissivo por padrão e erro apenas em modo estrito;
- JUnit não substitui JSON de benchmark nem descarta amostras;
- falha de asserção, timeout e crash não viram o mesmo estado;
- CI compartilhado não torna benchmark ruidoso obrigatório antes de SL_T.6;
- a extensão não depende do texto ou das cores do terminal.

## Contratos implementados

### SL_T.5A — Baseline e comparação

- `arandu bench --save-baseline <nome>` publica atomicamente em
  `target/arandu/benchmarks/<nome>.json`;
- `--compare`/`--baseline` aceita ausência por padrão; `--strict` exige o
  documento e o mesmo conjunto/configuração de casos;
- nomes de baseline são validados e não podem escapar do diretório;
- `--dry-run` apresenta regressões sem falhar o processo;
- ambiente inclui alvo, backend, perfil, relógio, SO, arquitetura e CPU. O
  override `ARANDU_BENCH_MACHINE` permite identidade explícita de runner;
- ambientes incompatíveis retornam código 4; baseline estrito ausente, 3;
  regressão ou falha do benchmark, 1; sucesso, 0.

### SL_T.5B — Decisão robusta

- comparação usa mediana e MAD das amostras;
- `--max-regression` define diferença prática permitida;
- `--noise-threshold` define o piso de ruído;
- o limiar efetivo é o maior entre a política e a incerteza observada nos dois
  conjuntos. Resultado é `improved`, `unchanged` ou `regressed`;
- JSON preserva amostras, estatísticas, ambiente, limiares e classificação.

Isso é uma decisão conservadora por dispersão robusta, não uma alegação de
significância científica universal. Perf counters e bootstrap avançado ficam
fora da primeira Gold.

### SL_T.5C — JUnit portátil

- `arandu test --format junit --output <arquivo>`;
- `<failure>` representa asserção; `<error>` representa timeout/crash;
  `<skipped>` preserva skip;
- IDs determinísticos formam `classname` + `name`;
- XML escapa dados do usuário e substitui controles inválidos;
- stdout/stderr e truncamento são preservados;
- JSON permanece o formato de integração com semântica completa.

### SL_T.5D — CI

- `S0 / Gate` executa um projeto real e publica o XML como artifact;
- isso ocorre no workflow existente, sem duplicar a suíte;
- benchmarks continuam informativos. Um gate de regressão nativo pertence à
  campanha SL_T.6, após baseline estável por sistema.

### SL_T.5E — VS Code

- Test Explorer descobre casos via `arandu test --list` em background;
- execução usa `--exact --format json` e mapeia passed, failed, skipped,
  timed_out e crashed para estados da Testing API;
- output usa CRLF exigido pelo terminal do Test Explorer;
- alterações `.aru`/manifest são coalescidas antes da redescoberta;
- `Arandu: Run Benchmark` lista IDs canônicos e executa a CLI;
- `arandu.cli.path` permite configuração explícita; descoberta também cobre
  SDK instalado e binários de desenvolvimento.

## Evidências e promoção

- testes unitários cobrem mediana/MAD, piso de ruído, nomes hostis e XML;
- E2E de CLI cobre JUnit, save, compare e baseline estrito ausente;
- Extension Host prova comandos e descoberta real no Test Explorer; a campanha
  SL_T.6 deve provar o fluxo completo com SDK público nos três sistemas;
- A sequência completa do `AGENTS.md`, os testes de query e o Extension Host
  passaram localmente. A matriz de SDK público permanece em SL_T.6.

## Fora do SL_T.5

- coverage entre backends;
- testes async;
- benchmarks distribuídos ou entre máquinas incompatíveis;
- perf counters específicos de SO;
- baseline remoto mutável ou atualização automática por PR;
- gate obrigatório de desempenho em runner compartilhado.
