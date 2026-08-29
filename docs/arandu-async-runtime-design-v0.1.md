# Arandu — Async Runtime Design (SL_R) v0.1

**Status:** SL_R.0–.3 + SL_R.1 host surfaces implemented. Consumes A3 compiler semantics.

| Layer | Owns | Does not own |
|-------|------|----------------|
| **Compiler (A3)** | `async`/`await`, `Coroutine[T]`, `Poll[T]`, suspend CFG | threads, epoll, spawn queues |
| **`std.core.future`** | `Poll` enum | OS reactor |
| **`std.runtime` (SL_R)** | Executor, reactor, waker, sockets, supervisor | language `await` syntax |

---

## Visão Geral e Contexto

O contrato separa semântica `async` do compilador, tipos fundamentais e runtime
de host, evitando que scheduler ou I/O vazem para a linguagem/IR.

## Detalhes Técnicos da Implementação

### Shipped surfaces

### SL_R.0 — SyncExecutor + Coroutine

```text
spawn_int / join_int / block_on_int   // multi-file reliable (concrete)
spawn<T> / join<T> / block_on<T>      // generic; prefer explicit type args for mono
```

ABI: `Coroutine[T]` is a state-blob pointer; `job as ptr[u8]` is the host bridge.

### SL_R.2 / SL_R.3 — Reactor

- `EpollReactor` + sleep/arm/poll
- `reactor_backend()`: **0** portable, **1** epoll, **2** io_uring (runtime detect)
- Sleep prefers io_uring timeout when backend is 2, else epoll+timerfd

### Waker / Context

- `Waker`, `new_waker`, `waker_wake`, `waker_wait`, `destroy_waker`
- `Context` holds a `Waker` (explicit, no global)

### TCP sockets (blocking host MVP)

- `tcp_listen` / `tcp_accept` / `tcp_connect` / `tcp_read` / `tcp_write` / close

### SL_R.1 — Supervisor

- `Supervisor` + `supervisor_spawn(path, max_restarts)` / `poll` / `wait` / `kill`
- Worker processes bound blast radius under process abort policy

---

### Gold decisions (unchanged)

1. **No global executor** — all handles are explicit values.
2. **Runtime backend select** — io_uring when kernel allows, else epoll.
3. **Isolation via processes** — not catch_unwind; supervisor restarts workers.

---

### Honesty

| Item | Status |
|------|--------|
| Multi-file inferred `rt.spawn` / `rt.join` | Done (namespace generic infer + HIR specialized types) |
| Same-module inferred `join_g` mono | Done |
| TCP nonblocking + `tcp_wait` / `tcp_wait_wake` | Done |
| io_uring read/write when backend=2 | Done (`tcp_read_async` / `tcp_write_async`) |
| Full Future trait on Coroutine | Open (Waker/Context handles exist) |

## PONTOS DE MELHORIA (O que não está no roadmap)

O Future trait completo permanece aberto e as superfícies de host não implicam
paridade em todos os targets. Esse limite deve continuar explícito.

## Futuro e Próximos Passos

Ordenar a ampliação pelo roadmap SL_R/SL_S e validar scheduler, cancelamento,
I/O e shutdown por sistema operacional antes de promoção Gold.
