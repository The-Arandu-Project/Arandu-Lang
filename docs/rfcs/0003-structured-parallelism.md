# RFC 0003: Processamento Paralelo Estruturado e Worker Pool Bounded

- **Número da RFC:** 0003
- **Título:** Concorrência e Processamento Paralelo Estruturado via Worker Pool Bounded
- **Autor(es):** Equipe do Compilador Arandu
- **Data de Início:** 2026-09-11
- **Status:** `Implemented`
- **Área Principal:** `Runtime` / `Middle-end` (`arandu_mir` / `arandu_runtime`)

---

## 1. Resumo (Summary)

Esta RFC define o modelo de concorrência e paralelismo estruturado do Arandu. Nenhuma tarefa ou worker filho pode escapar do escopo léxico que a instanciou. Erros, falhas e cancelamentos são tratados de modo determinístico no limite do escopo, eliminando vazamentos de tarefas (*detached tasks*), contenção descontrolada de threads e corridas de dados (*data races*).

---

## 2. Motivação (Motivation)

Sistemas concorrentes tradicionais sofrem com duas armadilhas antagônicas:
1. **Threads e Promises Destacadas (Go goroutines, JS/Node Promises, C++ `std::thread`)**: Tarefas filhas continuam executando em segundo plano mesmo após a função pai retornar ou falhar, causando vazamentos silenciosos de recursos, acessos a ponteiros inválidos e consumo desgovernado de memória.
2. **Concorrência com Locks Explícitos (Pthreads, Rust `Mutex<T>`)**: Degradação severa de escalabilidade sob alta concorrência por contenção de cache (*cache bouncing*), deadlocks e não determinismo na ordem de execução.

O Arandu resolve esses problemas através de **Paralelismo Estruturado**: as tarefas filhas são confinadas pelo sistema de tipos (`Send` + `Sync` bounds) e pelo ciclo de vida do bloco léxico. A redução dos dados ocorre através de operadores associativos determinísticos.

---

## 3. Topologia e Fluxo de Execução

```text
Entrada (fatia []T ou coleção contígua)
           │
           ▼
 particionamento fixo pela entrada
 (independente de workers)
           │
           ▼
  WorkerPool bounded ──► [Worker 0] ... [Worker N] (WorkThunk ABI)
           │
           ▼
  redução ordinal associativa (Combine<R>)
           │
           ▼
 resultado estável entre contagens de workers
```

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

### Contrato de ABI e Transporte de Tarefas

Todo trabalho paralelo atravessa a fronteira entre compilador e runtime via ponteiro com convenção C ABI:
```c
int32_t (*ar_work_thunk)(void *context, void *result);
```
* `context`: Ponteiro para a struct contendo o subintervalo de dados e argumentos de entrada.
* `result`: Ponteiro para a região onde o resultado do chunk será gravado pelo worker.
* Retorno: `0` para sucesso, `2` para cancelamento e outro código não zero para falha controlada.

### Inlining de Funções-Folha na AMIR

Para evitar a sobrecarga de chamadas indiretas no laço quente de processamento paralelo, o compilador introduz um passador de inlining na AMIR (`arandu_mir::inlining`) com orçamento estrito:
* Apenas funções-folha (*leaf functions* sem chamadas aninhadas);
* Máximo de 25 instruções e 6 blocos básicos;
* Ausência de loops e corrotinas;
* Preservação garantida da forma SSA/OSSA.

---

## 5. Invariantes de Arquitetura

1. **Escopo Imutável**: Um grupo paralelo não pode concluir sua execução até que todas as tarefas admitidas tenham terminado ou sido cooperativamente canceladas.
2. **Partição e Ordem Estáveis**: Em `parallelFold`, os limites de chunks derivam apenas da entrada e os parciais são combinados em ordem ordinal. Alterar somente a contagem de workers não altera a árvore de redução.
3. **Contrato Algébrico Explícito**: `Combine` deve ser associativo, possuir a identidade fornecida como elemento neutro e ser estável sob o reagrupamento. Isso não implica igualdade com fold linear para aritmética IEEE-754 comum.
4. **Limitação de Admissão (*Bounded Admission*)**: A fila possui capacidade finita. Chamadas externas aguardam admissão; submissões aninhadas feitas por workers executam inline para impedir deadlock e crescimento ilimitado.

O runtime Rust usa workers persistentes. O backend C mantém o mesmo escopo e
critério de falha, mas ainda cria e junta threads nativas em cada chamada; a
reutilização equivalente permanece requisito de promoção a `gold`.
