# Arandu — Processamento Paralelo Estruturado v0.1

**Estado:** `done`; implementação de Fases 1 a 7, runtime de workers limitados, ABI C/Cranelift, stdlib `std.core.parallel`, inlining automático no AMIR e integração Pypor concluídos e verificados. Promoção a `gold` condicionada à validação na matriz de release Windows/macOS.

## Visão Geral e Contexto

O processamento paralelo estruturado provê execução paralela com segurança de memória e ciclo de vida confinado: nenhuma tarefa ou thread filha pode sobreviver ao escopo pai que a criou. Erros e cancelamentos são tratados de modo determinístico no limite do escopo, garantindo previsibilidade mesmo sob falhas de hardware ou cancelamento antecipado.

As decisões arquiteturais foram confrontadas diretamente com as abordagens de mercado, adotando as melhores práticas e evitando armadilhas conhecidas:

- **Swift SE-0304 (Structured Concurrency):** Escopo estruturado estrito onde tarefas filhas não escapam do grupo. Evitou-se o modelo de tarefas destacadas (*detached tasks*) que vazam recursos.
- **Java `StructuredTaskScope` (JEP 453):** Ciclo de vida confinado ao bloco léxico. Evitou-se a falha do JDK-8311867 (onde uma tarefa admitida entre `shutdown()` e o cancelamento escapava da interrupção) por meio de verificação atômica pré-admissão combinada com o token de cancelamento.
- **Go `errgroup`:** Limite de admissão estrito para proteger o sistema contra sobrecarga de concorrência. Evitou-se o erro da estrutura padrão do Go (`Group{}` ilimitado que acumula memória sem controle).
- **.NET `Task.WhenAll`:** Agrupamento determinístico de resultados. Evitou-se a armadilha do .NET de re-lançar apenas a primeira exceção por corrida não determinística; o Arandu registra e consolida resultados em ordem estável.
- **Rayon / C++26 `std::execution`:** Redução paralela e particionamento contíguo de fatias sem alocação por item. Evitou-se a cerimônia excessiva de montagem de *senders/receivers*.
- **Inlining Automático Orçado no AMIR:** Evitou-se o *code bloat* descontrolado de compilers C++/LLVM e a fragilidade *mid-stack* do Go através de um modelo estrito de funções-folha (*leaf functions*) com teto de 25 instruções e 6 blocos básicos, sem loops, corrotinas ou chamadas aninhadas.

```text
Entrada (fatia []T ou coleção)
           │
           ▼
    particionamento
   disjunto em chunks
           │
           ▼
  WorkerPool bounded ──► [Worker 0] ... [Worker N] (WorkThunk ABI)
           │
           ▼
  redução ordinal associativa (Combine<R>)
           │
           ▼
   Resultado determinístico e bit-idêntico
```

---

## Detalhes Técnicos da Implementação

### Responsabilidade por Camada

| Camada | Responsabilidade |
| :--- | :--- |
| `arandu_typeck` | Bounds canônicos `Send` e `Sync` via `LangItem`. Rejeição de tipos com referências ativas, corrotinas ou destruidores em fronteiras de thread. |
| `arandu_middle` | Definição de layout, ABI de rvalues, terminadores e contratos de funções e tipos compartilhados. |
| `arandu_mir` | Otimizações, preservação de SSA/OSSA e inlining automático de funções-folha (`arandu_mir::inlining`) com splicing puro de CFG e remapeamento denso de ranges. |
| `arandu_runtime` | `WorkerPool`, escalonador cooperativo com *self-help*, canal de admissão sincronizado (`sync_channel`), tokens de cancelamento cooperativo e suporte a thunks de tarefas (`ar_rt_parallel_fold_run`). |
| `arandu_backend_cranelift` | Tradução de chamadas C ABI, JIT builder com registro de símbolos runtime, resolução de tipos de agregados em memória e materialização de cópia de structs por valor. |
| `arandu_backend_c` | Emissor C com paridade exata para o layout de agregados `is_memory` e ponteiros de contexto/resultado `(ptr[C], ptr[R]) -> i32`. |
| `stdlib` | Módulo `std.core.parallel` com a função pública `parallelFold`, interfaces `ParallelJob<T, R>` e `Combine<R>`. |
| `pypor` | Consumidor de ponta a ponta: particionamento e contagem concorrente de código, comentários e linhas em branco. |

---

### Contrato de ABI e Transporte de Tarefas

1. **Assinatura do Thunk de Trabalho:**
   Todo trabalho paralelo atravessa a fronteira entre compilador e runtime via ponteiro C ABI:
   ```c
   int32_t (*ar_work_thunk)(void *context, void *result);
   ```
   Retornos de status:
   - `0`: Sucesso (`WORK_COMPLETED`). O buffer `result` contém o valor de retorno inicializado.
   - `1`: Falha na execução da tarefa.
   - `2`: Cancelado antes ou durante a execução (`WORK_CANCELED`).

2. **Self-Help e Prevenção de Deadlock:**
   Quando a fila de admissão atinge o limite máximo (`admission_bound`), threads de worker que tentam submeter tarefas não bloqueiam: executam a tarefa inline (*worker self-help*), garantindo progresso mesmo com dependências aninhadas.

3. **Inlining Automático de Funções-Folha (AMIR):**
   Pequenas funções utilitárias (como testes de caracteres e predicados) são automaticamente inlinadas nos callers antes do laço de fixpoint do otimizador:
   - **Elegibilidade:** Apenas funções-folha (sem chamadas a outras funções), sem terminadores `Suspend` e sem ciclos no CFG (detectados via DFS de 3 cores).
   - **Orçamento:** Custo de instruções $\le 25$, blocos básicos $\le 6$, máximo de 32 inlines por função chamadora.
   - **Splicing SSA:** O registrador de retorno `TempId(0)` da callee é mapeado diretamente para o registrador SSA de destino do caller (`call.lhs`), os blocos intermediários são inseridos e as tabelas de statements e parâmetros de bloco são reconstruídas de forma contígua e densa.
   - **Sinergia com Passos Existentes:** Após o splice, `simplify_cfg` funde os blocos sequenciais e `sccp` dobra constantes diretamente nos locais de uso.

---

## Evidência Experimental e Benchmarks

### Corpus de Validação
- **Repositório:** Árvore do Kernel Linux 6.x (`benchmarks/linux`).
- **Dimensão:** 65.370 arquivos físicos, 37.900.970 linhas de código.
- **Hardware:** Linux x86_64, 16 CPUs lógicas.

### Resultados de Desempenho (`pypor` Release)

| Configuração | Tempo Real (s) | Tempo Usuário (s) | Tempo Sys (s) | Utilização de CPU (%) | Max RSS (MB) | Taxa (M linhas/s) |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| `pypor (--seq)` | 16.81s | 16.33s | 0.48s | 100.0% | 76.7 MB | 2.25 M/s |
| `pypor (workers=1)` | 47.02s | 24.55s | 2.47s | 57.5% | 101.2 MB | 0.81 M/s |
| `pypor (workers=2)` | 6.83s | 11.99s | 0.45s | 182.0% | 61.5 MB | 5.55 M/s |
| `pypor (workers=4)` | 3.10s | 9.65s | 0.36s | 322.7% | 63.3 MB | 12.22 M/s |
| `pypor (workers=8)` | **1.83s** | 8.59s | 0.38s | **490.9%** | **71.2 MB** | **20.76 M/s** |

### Conclusões das Medições
1. **Escalabilidade Real:** Com 8 workers, o tempo de contagem de quase 38 milhões de linhas cai de 16.81s para **1.826s** (aceleração de 9.2x sobre o modo sequencial).
2. **Eficiência de Memória:** O consumo de memória (RSS) permanece estável em **71.2 MB**, inferior inclusive ao modo sequencial devido à libertação contínua de buffers de chunk.
3. **Paridade com Inlining Manual:** O inlining automático no AMIR eliminou 100% da sobrecarga de chamadas de predicados no hot-loop sem necessidade de inlining manual pelo desenvolvedor.
4. **Determinismo:** Todos os modos (sequencial e paralelo com 1, 2, 4 ou 8 workers) produziram exatamente os mesmos números totais:
   - Arquivos: `65.370`
   - Código: `28.583.594`
   - Comentário: `4.636.283`
   - Branco: `4.681.093`
   - Total: `37.900.970`

---

## Dívida Técnica Classificada e Futuro

1. **F2.5 — ABI de Agregados por Valor na Stack do JIT:**
   Atualmente, structs nomeadas no Cranelift JIT são alocadas como ponteiros de heap em `materialize_ptr_read_copy`. O passo F2.5 migrará agregados para homes na stack do frame de chamada, alinhando completamente com o modelo `is_memory` do backend C e eliminando alocações temporárias.
2. **Generalização do Tipo de Retorno `R`:**
   Atualmente o retorno de chunks suporta tipos `Copy` ou com destruidor explícito; a generalização completa para tipos arbitrários `Clone` sem destruidor requer a propagação de `PayloadDropGlue` para o resultado parcial.
3. **Validação Multiplataforma (Gate de Release):**
   Exercitar os testes de paralelismo e `WorkerPool` nos runners nativos de CI Windows e macOS durante os testes de release `rc.5`.
