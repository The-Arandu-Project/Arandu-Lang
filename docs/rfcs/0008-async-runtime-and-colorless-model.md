# RFC 0008: Runtime Assíncrono e Modelo Colorless (SL_R / A3)

- **Número da RFC:** 0008
- **Título:** Runtime Assíncrono Cooperativo e Modelo Colorless de Corrotinas
- **Autor(es):** Equipe do Compilador Arandu
- **Data de Início:** 2026-08-20
- **Status:** `Implemented` (Fases SL_R.0 a SL_R.3 na stdlib e compilador)
- **Área Principal:** `Runtime` / `Frontend` / `Compiler Infrastructure` (`A3`, `SL_R`)
- **Documento Relacionado:** `docs/arandu-async-runtime-design-v0.1.md`

---

## 1. Resumo (Summary)

Esta RFC especifica a infraestrutura de computação assíncrona do Arandu, composta pelo modelo semântico no compilador (**Fase A3**) e pela biblioteca de runtime cooperativa na stdlib (**Fase SL_R**).

A proposta isola rigidamente a semântica da linguagem (`async`/`await`, `Coroutine[T]`, `Poll[T]`, suspensão de CFG) dos detalhes do sistema operacional (threads, `epoll`, `io_uring`, sockets), permitindo que código assíncrono seja portável entre ambientes nativos e WebAssembly sem poluição cromática de funções (*What Color is Your Function*).

---

## 2. Separação Estrita de Responsabilidades

| Camada | É Dono de | NÃO é Dono de |
| :--- | :--- | :--- |
| **Compilador (`A3`)** | `async`/`await`, `Coroutine[T]`, `Poll[T]`, CFG de Suspensão | Threads do SO, epoll, filas de spawn |
| **`std.core.future`** | Enum `Poll[T]` (Ready / Pending) | Reator do SO |
| **`std.runtime.executor`** | Executor cooperativo (`SyncExecutor`, `TaskHandle<T>`, `spawn`, `join`, `blockOn`, `cancel`) | Reator do SO, wakers, sockets |
| **`std.runtime.waker`** | `Waker` / `Context` (token de notificação) | Sockets de rede |
| **`std.runtime.reactor`** | Reator do SO (Linux `epoll` / `io_uring`, macOS `kqueue`) | Executor, sockets |
| **`std.net`** | Sockets TCP e abstrações de I/O de rede | Reator, wakers |

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

No código Arandu, funções que realizam operações assíncronas são definidas e consumidas de maneira direta:

```arandu
import std.net as net
import std.runtime.executor as exec

func fetch_data(addr: str): int {
    let sock = net.connect(addr);
    let bytes = sock.read();
    return bytes.len();
}

func main(): int {
    // Execução síncrona/bloqueante via executor cooperativo
    let handle = exec.spawn(fetch_data("127.0.0.1:8080"));
    let result = exec.blockOn(handle);
    return result;
}
```

---

## 4. Explicação em Nível de Referência: Transformação na AMIR

O compilador transforma funções assíncronas em máquinas de estado determinísticas baseadas em blocos básicos da AMIR:
1. **Ponto de Suspensão (`Suspend`)**: A instrução `await` é reduzida a um terminador de controle `Suspend` na CFG.
2. **Salvamento de Estado**: Variáveis locais vivas através do ponto de suspensão são projetadas para o frame de estado da corrotina.
3. **`RelativeBorrow`**: Referências que cruzam pontos de suspensão são transformadas em empréstimos relativos indexados no frame (`LocalId`), impedindo corrupção de ponteiros quando a corrotina é retomada por outro worker.
4. **Proibição de Empréstimos de Pilha Trans-Suspensão (`O010`)**: O compilador emite erro fatal se um empréstimo puro de pilha tentar atravessar um ponto de suspensão, garantindo que nenhum ponteiro solto sobreviva à liberação temporária da thread.

---

## 5. Invariantes de Arquitetura

1. **Ausência de Efeitos Ocultos na IR**: A AMIR e o type checker não realizam chamadas ao sistema operacional ou alocações opacas de threads durante a compilação de código assíncrono.
2. **Tipagem Estrita de Handles**: `TaskHandle<T>` carrega o tipo do resultado inferido diretamente de `Coroutine[T]`. Tentar esperar um resultado incompatível resulta em erro de checagem de tipos estático.
