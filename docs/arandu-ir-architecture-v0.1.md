# Arandu Compiler IR Architecture v0.1

> **Roadmap e checklist executivo:** [arandu-compiler-roadmap-v0.1.md](./arandu-compiler-roadmap-v0.1.md)

This document defines the intermediate representation (IR) strategy for the Arandu compiler. To keep the compiler development pragmatic, modular, and maintainable, Arandu strictly separates semantic analysis from control flow and memory analysis using a two-level IR architecture.

---

## Visão Geral e Contexto

Este documento registra por que o compilador separa AST, AHIR/HIR e AMIR e
quais responsabilidades podem cruzar cada fronteira.

## Detalhes Técnicos da Implementação

### 1. Compiler Pipeline

The compilation pipeline for an Arandu source file follows a structured phase-by-phase layout:

```text
Source Code (.aru)
       ↓
     Lexer       (Tokenizes source code)
       ↓
    Parser       (Constructs AST)
       ↓
   Resolver      (Name resolution, symbol definitions)
       ↓
 Type Checker    (Infers & validates types, populates TypeInfo)
       ↓
   Lowering      (Converts AST + TypeInfo to AHIR)
       ↓
   AHIR v0.1     (High-level Intermediate Representation / Typed AST)
       ↓
   AMIR v0.1     (Mid-level Intermediate Representation / CFG & OSSA)
       ↓
Borrow Checker   (Ownership, lifetimes, and generational checks)
       ↓
  Backend IR     (Lowered instructions for code generation)
       ↓
 Code Gen Targets (C / Cranelift / LLVM)
```

---

### 2. AST vs AHIR vs AMIR

> **Implementation note (C5):** In the `arandu_semantics` crate, **AHIR** is implemented as the Rust module `hir` (`HirProgram`, `lower_to_hir`, CLI `arandu hir`). Documentation and CLI may say “AHIR” or “HIR”; they refer to the same IR.

The intermediate representations serve distinct purposes at different stages of the compiler:

| Feature / Goal | AST (Abstract Syntax Tree) | AHIR (Arandu High-level IR) | AMIR (Arandu Mid-level IR) |
| :--- | :--- | :--- | :--- |
| **Primary Goal** | Syntax validation | High-level semantics & Types | Flow, memory & code gen |
| **Structure** | Tree (matching source) | Tree (typed, resolved) | Control Flow Graph (CFG) |
| **Names** | Unresolved strings | Resolved `SymbolId` / `LocalId` | SSA Temp registers (`_0`, `%0`) |
| **Types** | Unresolved (`TypeExpr`) | Fully resolved (`ArType`) | Lowered representation types |
| **Memory / Safety** | None (implicit/syntax only) | None | Explicit Ownership SSA (OSSA) |

---

### 3. AHIR v0.1 Responsibilities

AHIR v0.1 serves as the **Typed AST**. It represents the program semantically while maintaining tree structure and source-level constructs.

### Scope of AHIR

- **Fully Resolved Names**: Every identifier reference is resolved to its defined `SymbolId`.
- **Fully Inferred/Checked Types**: Every expression and variable binding carries its concrete `ArType`.
- **Generics & Interfaces**: Preserves structural interface constraints, generic parameters, and `where` clauses in their source-level semantic form.
- **Result/Option Constructors**: `Result.Ok`, `Result.Err`, and `Option.Some` are represented semantically before AMIR lowering.
- **Diagnostics Context**: Preserves source spans (`Span`) to emit high-quality diagnostics close to the user's code.

### Excluded from AHIR

- **No CFG**: Loops, conditions, and logical evaluations remain as nested tree statements.
- **No Borrow Checking**: Lifetime analysis and mutation conflicts are not evaluated here.
- **No Lowering**: Features like `defer`, `errdefer`, catch blocks, and safe-navigation (`?`) are preserved as-is.

---

### 4. AMIR Responsibilities

AMIR represents the program as a Control Flow Graph (CFG) with explicit control-flow blocks and dataflow edges. It is designed to perform dataflow analysis and enforce memory safety.

### AMIR v0.1 (Current)

- **Explicit Control Flow Graph (CFG)**: Lowers `if`, `while`, `for`, and control flow into basic blocks and jumps.
- **Local Registers**: Every variable and temporary is a numbered local (`_0`, `_1`, ...) with explicit type.
- **Place Projections**: Supports nested field and index mutations via `AmirPlace` with projections.
- **Basic Expressions**: Literals, binary/unary ops, field access, index access, array literals, struct literals, calls.
- **Lowered in AMIR:** `match`, `if is`, `defer`/`errdefer`, `?`, `?.`, `?[]`, `??`, `for in`, `alloc`, and `free`.
- **Result/Option Representation:** Source-level `Result<T,E>` and `Option<T>` lower through `ResultCtor`/tuple ok-err layout without legacy source-level tuple fallback rules.
- **Definite Initialization:** `passes/definite_init.rs` runs a CFG dataflow analysis and reports O008 for possibly uninitialized local reads.
- **OSSA ownership:** AMIR models `copy`, `move`, `borrow`, `borrow_mut`, `destroy`, `StorageLive`, and `StorageDead`. BV.1 derives a stable structural `ReturnBorrowSummary` from typed AMIR flow, unions only origins that reach each result path, solves recursive calls to fixpoint and carries the converged contract on calls. The narrow `borrow_interfaces` Salsa query cuts off body-only changes while changed contracts invalidate importers. AUD.4 keeps IDE analysis aligned and implicit `Destroy` is elaborated before M2 so O006 observes semantic AMIR. The system remains partial until BV.2 propagates every aggregate holder and BV.3 publishes safe `Slice`/`String` views.
- **Still unsupported in AMIR lowering:** `catch` (roadmap v0.2 CATCH), unsafe block expressions (v0.2 UNSAFE), lambdas (v0.3 LAMBDA), and async blocks/`await` (v0.3 ASYNC).

### AMIR v0.2+ (Future)

- **Move Checker Pass**: Promotes the current move/copy annotations into O001/O005/O007 diagnostics over the CFG.
- **Borrow OSSA extensions**: extend the direct AUD.3 return summary to conditional non-escapable carriers such as `Option<ref T>`, `Slice` and string views; the existing direct shared/mutable return and conflict checks are active.
- **Catch Lowering**: Desugars catch-handlers into explicit CFG branches.
- **Generational Fallback**: Inserts transparent runtime checks where the static ownership model intentionally falls back.
- **Unsafe Lowering**: Validates unsafe-only operations and lowers unsafe block expressions.

---

### 5. Deferred Features

To ensure steady progress and prevent overengineering, several advanced compiler design patterns are deferred to later versions:

1. **Kotlin-style Query System / Incremental IDE Backend**:
   - *Strategy*: Perform linear batch processing (whole-program or file-by-file). Query architectures and incremental compilation are deferred to `v0.3+`.
2. **Zig-style Comptime**:
   - *Strategy*: Evaluation of compile-time expressions is deferred to `v0.4/v0.5`. Comptime requires a stable type checker and lowering backend before implementation.
3. **GHC-style Core Rewrite Rules**:
   - *Strategy*: We avoid high-level algebraic optimization rules in favor of direct lowering and backend-level optimizations.
4. **Stable Public AHIR/AMIR API**:
   - *Strategy*: All internal representations are unstable and can be changed without warning until the compiler can check and compile small, end-to-end programs.

---

### 6. Pretty-Printing Contract

The `arandu hir` subcommand outputs a pretty-printed representation of the AHIR. This format is designed for readability, debugging, and verification via golden tests.

### Contract Rules

1. **Source/AST Order**: The pretty printer outputs declarations and statements in the exact order they appeared in the source file.
2. **Deterministic Output**: For structures without a natural AST order (like member sets or generic associations), elements must be sorted alphabetically by name before printing.
3. **Indentation**: Indentation uses two spaces (`  `) per level to represent hierarchical nesting.
4. **Expression Types**: Every expression must suffix its resolved type (e.g. `LocalRef(a): int` or `Binary(+): int`).

### Pretty-Printing Examples

#### Module

```text
Program
  Module examples.stable.syntax.hello
```

#### Function with Binary Expression

```text
Func add(a: int, b: int) -> int
  Return
    Binary(+): int
      LocalRef(a): int
      LocalRef(b): int
```

#### Structs

```text
Struct User
  id: int
  name: str
  email: str?
```

#### Enums

```text
Enum LoadState
  Idle
  Loading
  Success(User)
  Failed(Err)
```

#### Constants

```text
Const maxRetries: int =
  Int(3): int
```

## PONTOS DE MELHORIA (O que não está no roadmap)

Os itens deferred acima são extensões, não permissão para fundir IRs. Mudanças
devem evitar clones profundos e projeções monolíticas no caminho incremental.

## Futuro e Próximos Passos

Promover novos IR features apenas pelo roadmap mestre, acompanhados de
invariantes, visitors completos e testes de paridade entre backends.
