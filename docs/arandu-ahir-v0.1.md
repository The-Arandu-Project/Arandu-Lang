# Arandu High-level Intermediate Representation (AHIR) v0.1

AHIR v0.1 is a typed, name-resolved syntax tree representation of the Arandu language. It serves as the bridge between semantic analysis (type checking, symbol resolution) and lower-level control flow graphs.

## Visão Geral e Contexto

AHIR/HIR é a representação tipada e resolvida que separa a sintaxe do
programa das análises de controle, ownership e backend.

## Detalhes Técnicos da Implementação

### What AHIR Represents

1. **Fully Resolved Names**: Every reference to a variable, parameter, struct, enum variant, or function is resolved to a unique `SymbolId` from the `SymbolTable`.
2. **Explicit Concrete Types**: Every expression and variable declaration carries its resolved `ArType`. No implicit widening remains; all type promotions/coercions must be explicitly represented.
3. **AST Structure**: The structure corresponds directly to the source code hierarchy (nested loops, conditions, and statements remain as trees).

### What AHIR Does NOT Represent

1. **No Basic Blocks or CFG**: Control flow elements (`if`, `while`, `for`, `match`) are not flattened into jumps.
2. **No Memory Ownership Lifetimes**: Lifetime analysis, borrowing, and moving are not represented or enforced in AHIR.
3. **No Desugaring**: Features like `defer`, `errdefer`, and safe-navigation (`?.`, `?[]`) are preserved structurally.

### Pretty-Printing Contract

The pretty-printed representation of AHIR (output by `arandu hir`) complies with the following format:

1. **Indentation**: 2 spaces per indentation level.
2. **Source Order**: Elements must be printed in the exact order they appeared in the source code.
3. **Deterministic Collections**: Associated structures (such as struct fields, enum variants) that do not have a defined source-level statement order are sorted alphabetically before printing.
4. **Types**: Every expression node is printed with its evaluated type as a suffix (e.g. `Int(5): int` or `LocalRef(idx): int`).

### Structural Examples

### 1. Func Declaration with Binary Expression

```text
Func add(a: int, b: int) -> int
  Return
    Binary(+): int
      LocalRef(a): int
      LocalRef(b): int
```

### 2. Struct Definitions

```text
Struct Point
  x: int
  y: int
```

### 3. Enum Definition

```text
Enum LoadState
  Idle
  Loaded(str)
```

### 4. If Expression

```text
If: int
  Binary(>): bool
    LocalRef(res): int
    Int(0): int
  Then
    Expr
      Int(10): int
  Else
    Expr
      Int(20): int
```

### 5. Match Expression

```text
Match: int
  LocalRef(state): LoadState
  Arm(Enum { variant: "Idle", ... }):
    Int(0): int
  Arm(Enum { variant: "Loaded", payload: [Bind { name: "s" }] }):
    Int(1): int
```

### 6. While Loop with Assignments

```text
While
  Binary(<): bool
    LocalRef(idx): int
    Int(5): int
  Set (idx) =
    Binary(+): int
      LocalRef(idx): int
      Int(1): int
```

### Relationship to the Type Checker

AHIR is constructed during the **Lowering** pass after type checking completes. The lowering pass takes the parsed AST, the resolved namespace definitions, and the `TypeInfo` produced by the type checker:

- Unresolved variable paths are replaced by `HirExprKind::Path` carrying the resolved `SymbolId`.
- Implicit type conversions are fully annotated.
- If type checking or name resolution finds any semantic errors (diagnostics of severity `Error`), the pipeline aborts, diagnostics are printed to standard error, and the compiler exits with status code `1` without generating or printing AHIR.

## PONTOS DE MELHORIA (O que não está no roadmap)

Pretty-printing ainda é uma superfície grande, mas coesa e usada como evidência
golden. O IR não deve receber efeitos de CLI, filesystem ou Salsa.

## Futuro e Próximos Passos

Preservar IDs e tipos estáveis por item e revisar qualquer crescimento com os
limites de invalidação antes de ampliar o IR.
