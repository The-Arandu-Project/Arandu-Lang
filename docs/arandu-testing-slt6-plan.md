# SL_T.6 — Campanha Gold

**Estado:** implementado localmente; promoção aguarda matriz e soak  
**Pré-requisito:** SL_T.0–SL_T.5 implementados  
**Resultado:** promover `SL_T` de campanha ativa para `gold`

## Objetivo

Provar que o harness publicado funciona como produto, não apenas dentro do
monorepo: instalar o SDK público em ambientes limpos, criar um projeto, descobrir
e executar testes e benchmarks, consumir os protocolos na extensão e preservar
determinismo, recuperação e segurança nos três sistemas suportados.

Gold significa que o contrato v1 é estável, utilizável e defendido por gates. Não
significa ausência de evolução futura nem transforma medições ruidosas em provas
de desempenho.

## Pesquisa e decisões

- O [GitHub Actions recomenda matrizes](https://docs.github.com/en/actions/how-tos/write-workflows/choose-what-workflows-do/run-job-variations)
  para variações de sistema e permite transportar artefatos entre jobs. Arandu
  reutilizará os pacotes produzidos uma vez; cada consumidor testará exatamente
  o archive correspondente ao alvo, sem recompilar uma cópia diferente.
- [Workflow artifacts](https://docs.github.com/en/actions/concepts/workflows-and-actions/workflow-artifacts)
  são evidência e transporte entre jobs, não cache. Digest, checksum interno e
  [attestation de proveniência](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations)
  permanecem verificáveis antes do teste black-box.
- A documentação do [VS Code Extension Host](https://code.visualstudio.com/api/working-with-extensions/testing-extension)
  permite instalar um VSIX antes do teste e recomenda desabilitar extensões
  alheias. O Gold exercitará a extensão empacotada contra o SDK instalado, não
  apenas `extensionDevelopmentPath` apontando para a árvore fonte.
- As [recomendações de benchmark do LLVM](https://llvm.org/docs/Benchmarking.html)
  exigem repetição e controle de frequência, serviços, CPU e armazenamento, e
  alertam que pouco ruído ainda não elimina viés. Assim, runners hospedados
  validam protocolo, cálculo e portabilidade; regressão temporal só será
  obrigatória em runner identificado e previamente calibrado.

Problemas de outras abordagens que este plano evita:

- reconstruir no job consumidor e acabar testando outro artefato;
- usar checkout, Cargo, Python ou variáveis internas como dependência oculta do
  usuário do SDK;
- chamar um benchmark isolado de regressão estatística;
- compartilhar baseline mutável entre máquinas incompatíveis;
- deixar um nome de job variável quebrar a regra de branch protection;
- duplicar toda a suíte Rust em S0, matriz Gold e release;
- validar a extensão somente com mocks ou carregada diretamente do fonte;
- promover Gold com flake conhecido, retry silencioso ou P0/P1 aberto.

## Contrato da campanha

### Matriz publicada

A matriz inicial coincide exatamente com os artefatos públicos existentes:

| Sistema | Alvo | Archive | Execução |
| --- | --- | --- | --- |
| Linux | `x86_64-unknown-linux-gnu` | `.tar.gz` | runner nativo |
| macOS | `aarch64-apple-darwin` | `.tar.gz` | runner nativo Apple Silicon |
| Windows | `x86_64-pc-windows-msvc` | `.zip` | runner nativo |

Nenhum teste emulado promove suporte a uma arquitetura. Um alvo novo entra no
contrato somente quando possuir pacote, instalação e campanha black-box nativa.

### Dois tipos de gate

1. **Correção do harness:** determinístico e obrigatório em toda a matriz.
2. **Regressão de desempenho:** informativo em runner compartilhado; obrigatório
   somente após existir runner dedicado, identidade estável e calibração aceita.

O check agregado terá nome fixo `SL_T / Harness`. Os nomes dos filhos podem
conter alvo, mas branch protection dependerá apenas do agregador.

## Plano de implementação

### SL_T.6A — Congelar e testar os contratos v1 — implementado

- Inventariar e congelar `arandu.test/v1`, `arandu.test-list/v1`,
  `arandu.bench/v1`, `arandu.bench-list/v1` e
  `arandu.bench-baseline/v1`, além do subconjunto JUnit e códigos de saída.
- Criar fixtures de compatibilidade que provem round-trip, campos obrigatórios,
  rejeição de major desconhecido e tolerância a campos aditivos.
- Garantir que IDs, caminhos e posições UTF-16 sejam independentes do checkout,
  separadores do host e ordem de descoberta.
- Centralizar as versões do protocolo no crate proprietário; a extensão apenas
  consome o contrato e falha claramente quando a major não é suportada.

**Aceite:** as mesmas fixtures passam no Rust e no TypeScript, sem parsing de
saída humana e sem duplicar descoberta de sintaxe no editor.

### SL_T.6B — SDK público black-box — implementado

- Extrair um workflow reutilizável que recebe os archives já produzidos pela
  matriz de distribuição e verifica digest/proveniência antes de instalá-los.
- Instalar em prefixo temporário fora de `GITHUB_WORKSPACE`; executar os comandos
  a partir de outro diretório e com `ARANDU_STDLIB`, `ARANDU_RUNTIME_LIB` e
  demais atalhos de desenvolvimento removidos.
- Limitar o `PATH` do processo consumidor ao SDK e às ferramentas básicas do SO.
  Rust, Cargo, Python e a árvore fonte podem existir no runner para preparar o
  teste, mas não podem ser necessários por nenhum comando público exercitado.
- Executar o ciclo `arandu new` → `check` → `build/run` → `test --list` →
  `test` humano/JSON/JUnit → `bench --list` → benchmark curto → salvar/comparar
  baseline.
- Verificar que `target/`, resultados e baselines ficam somente no projeto ou
  no caminho explicitamente solicitado.

**Aceite:** cada alvo publicado completa o mesmo cenário a partir do archive;
retirar o checkout depois da instalação não altera o resultado.

### SL_T.6C — Extensão empacotada — implementado

- Gerar o VSIX uma vez, instalar pela CLI do VS Code e iniciar um Extension Host
  limpo com extensões de terceiros desabilitadas.
- Apontar `arandu.cli.path` exclusivamente para o SDK da matriz e impedir fallback
  para `target/debug`, Cargo ou executáveis encontrados no repositório.
- Provar descoberta no Test Explorer, execução individual e total, skip, falha,
  timeout, cancelamento, navegação por localização e output estruturado.
- Provar `Arandu: Run Benchmark`, lista canônica e erro acionável quando CLI ou
  versão de protocolo estiverem ausentes/incompatíveis.

**Aceite:** o VSIX instalado funciona sobre um projeto recém-criado sem acessar
fontes do compilador ou implementar um analisador paralelo.

### SL_T.6D — Campanha adversarial e de recuperação — implementado

- Protocolo: frame truncado, tamanho excessivo, JSON inválido, major desconhecida,
  eventos duplicados/fora de ordem, stdout hostil e processo encerrado no meio.
- Processo: timeout, cancelamento, crash, árvore de filhos e descritores herdados;
  nenhuma espera infinita e nenhum processo órfão após o limite de graça.
- Filesystem: Unicode, CRLF, caminho longo, somente leitura, symlink/junction para
  fora do projeto, arquivo substituído e publicação interrompida.
- Concorrência: `--jobs 1` versus N, writers simultâneos de resultado/baseline e
  staging abandonado; publicação continua atômica e determinística.
- Reporters: XML com caracteres hostis/controles inválidos, truncamento marcado,
  zero casos, filtro vazio e output muito grande.
- Baseline: documento corrompido, conjunto divergente, ambiente incompatível,
  relógio não monotônico e amostras insuficientes.

Falhas injetadas usarão seams explícitos em testes; não serão reproduzidas por
timing arbitrário ou sleeps frágeis.

**Aceite:** toda falha termina em estado e código documentados, preserva o próximo
run e não vaza credenciais, caminhos incidentais ou bytes não determinísticos.

### SL_T.6E — Determinismo e calibração — implementado

- Repetir o corpus com seeds fixos, ordens de filesystem variadas e diferentes
  níveis de paralelismo; normalizar somente campos declaradamente voláteis.
- Comparar JSON/JUnit/listagens byte a byte onde o contrato é determinístico.
- Executar benchmark A/A antes de qualquer A/B e registrar `environment`, CPU,
  SO, alvo, backend, perfil, relógio, warmup, amostras, mediana e MAD.
- Calcular o envelope de ruído por máquina. Baseline não será atualizado por PR,
  nem aceito quando a identidade do ambiente divergir.
- Manter o job de regressão advisory nos runners hospedados. A passagem para
  obrigatório exige runner dedicado, série A/A estável documentada e política de
  manutenção/indisponibilidade.

**Aceite:** a campanha detecta regressões sintéticas acima do limiar e não acusa
regressão em A/A dentro do envelope observado.

### SL_T.6F — Workflow, evidências e custo — implementado

- Criar `.github/workflows/sl-t-harness.yml` reutilizável, com matriz nativa,
  `fail-fast: false`, permissões mínimas e concorrência que cancela apenas PRs
  obsoletos — nunca publicação iniciada.
- Produzir por alvo: JSON, JUnit, baseline/compare, manifesto de ambiente, logs de
  falha e resumo legível. Publicá-los como artifacts, não cache.
- O job agregador `SL_T / Harness` falha se um alvo obrigatório faltar, for
  cancelado ou falhar; não pode reportar verde com matriz parcial.
- `S0 / Gate` conserva apenas smoke rápido e determinístico. A matriz SL_T
  consome o produto do build e não repete `cargo test --workspace`.
- Release verifica a evidência do commit verde e testa o artefato público sem
  reexecutar desnecessariamente toda a suíte interna.

**Aceite:** um PR executa uma suíte interna e uma campanha black-box claramente
separadas; os artifacts permitem reproduzir e diagnosticar cada falha.

### SL_T.6G — Promoção Gold e consolidação — aguardando CI/soak

- Zerar defeitos P0/P1 do escopo e classificar explicitamente qualquer P2/P3.
- Obter pelo menos 10 execuções consecutivas verdes de toda a matriz durante no
  mínimo 7 dias, incluindo execução agendada e manual, sem retry que esconda
  flake. Falha de infraestrutura é registrada separadamente e não conta como
  evidência verde.
- Tornar `SL_T / Harness` obrigatório na `main` somente depois de o nome agregado
  e os tempos da campanha estarem estáveis.
- Atualizar o roadmap principal para `gold`, converter este plano em registro de
  decisão concluída e reduzir o documento-mãe ao contrato vivo.
- Documentar despromoção: quebra de protocolo v1, falha reproduzível em alvo
  publicado ou flake não contido reabre SL_T antes de nova release.

## Ordem de entrega

1. Contratos e fixtures (A).
2. Workflow reutilizável e SDK black-box (B + F parcial).
3. VSIX instalado (C).
4. Adversarial, recovery e determinismo (D + E).
5. Agregador, soak e promoção (F + G).

Cada etapa deve chegar com seus testes completos; não se promove um alvo por
inferência a partir de outro sistema operacional.

## Implementação local

- `xtask check-slt6-sdk` conduz o ciclo black-box fora do checkout, remove
  overrides de desenvolvimento e grava evidências JSON/JUnit por alvo;
- o coordenador propaga a stdlib já validada aos filhos e as compilações exatas
  reutilizam essa identidade, eliminando dependência acidental do diretório de
  trabalho descoberta pela campanha;
- contratos v1 de test/bench/list/baseline têm constantes no crate proprietário,
  framing adversarial em Rust e validação estrutural no cliente TypeScript;
- o Extension Host instala o VSIX candidato, configura o CLI/LSP do SDK e executa
  um teste real além de validar descoberta;
- `.github/workflows/sl-t-harness.yml` empacota o VSIX uma vez, executa a matriz
  nativa completa e publica um check agregado estável `SL_T / Harness`.

A implementação pode ser declarada Gold somente depois de o workflow remoto
passar e cumprir o soak de SL_T.6G; resultado local não substitui runners nativos.

## Riscos e guardrails

| Risco | Guardrail |
| --- | --- |
| CI verde testa o checkout, não o produto | instalar archive verificado fora do workspace e remover atalhos de desenvolvimento |
| Matriz parcial produz agregador verde | agregador enumera e exige todos os alvos publicados |
| Benchmark flaka a branch protection | correção é obrigatória; performance compartilhada é advisory |
| Baseline contaminado ou promovido pelo PR | baseline imutável, compatibilidade de ambiente e promoção separada |
| VSIX mascara dependência do monorepo | instalar o pacote e configurar somente o CLI do SDK |
| Teste de recovery depende de corrida | fault injection explícita, relógio/limites controlados |
| S0 fica ainda mais lento | manter apenas smoke; não repetir workspace tests na campanha |
| Novo alvo é anunciado cedo demais | package + instalação + black-box nativo são uma única condição de suporte |

## Validação da implementação

Além da sequência obrigatória do `AGENTS.md`, a implementação deverá executar:

- testes de compatibilidade de protocolo em Rust e TypeScript;
- E2E do CLI contra cada SDK instalado;
- Extension Host com o VSIX instalado;
- matriz black-box nativa completa;
- campanha de determinismo `--jobs 1` versus N;
- A/A de benchmark antes dos casos sintéticos A/B;
- verificação de artifacts, checksums e attestations;
- soak definido em SL_T.6G antes da promoção.

## Fora da primeira Gold

- tornar regressão de performance obrigatória em runner compartilhado;
- anunciar alvos sem runner nativo e pacote público;
- cobertura unificada entre backends;
- fuzzing geracional de programas Arandu;
- benchmarks distribuídos, perf counters portáveis ou comparação entre máquinas;
- testes async antes de `SL_R` fornecer scheduler, cancelamento e relógio testável.
