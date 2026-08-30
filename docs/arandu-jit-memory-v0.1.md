# JIT memory policy (v0.1 / debug Cranelift)

**Status:** intentional process-lifetime allocation for the **host debug JIT**.  
**Not** a production GC or ownership runtime.

## Visão Geral e Contexto

Este contrato torna explícita a política de memória do JIT Cranelift de debug
e evita confundi-la com o runtime de ownership da linguagem.

## Detalhes Técnicos da Implementação

### What allocates

| Source | Allocator | Lifetime |
|--------|-----------|----------|
| String literals in data section | module data | until `CompiledModule` drop |
| `ToStr` / string interp / `err.new` | `malloc` | process (leak OK in debug) |
| Struct / enum construct | `malloc` | process |
| Boxed `int?` / scalar `T?` | `malloc` | process |

### Guarantees (v0.2 Dev/Debug)

1. **Correctness over reclaim**: handles and fat pointers remain valid for the duration of the process after `run`.
2. **No double-free** in the happy path: JIT does not free user values yet; poison-on-free in debug is reserved for future ownership passes (OSSA M2).
3. **ABI**: `T?` is a null-or-pointer handle; scalars are boxed so payload `0` ≠ `nil`.

### Planned (not this doc)

- Arena tied to `CompiledModule` with batch free on drop, **or** bump allocator (`bumpalo`) per compile unit.
- OSSA-driven `Destroy` / `free` when ownership analysis is complete.

### C backend

`emit-c` may emit `malloc`/`free` stubs for portability; host C parity tests do not claim a freestanding allocator yet.

## PONTOS DE MELHORIA (O que não está no roadmap)

A alocação por tempo de processo é intencional no JIT de desenvolvimento,
mas precisa ser medida caso o JIT se torne superfície longa/embutida.

## Futuro e Próximos Passos

Definir liberação/isolamento por módulo somente com workload real; manter o
runtime de produção e o backend C como contratos separados.
