# OSSA Virtual Anchoring & Pruning

This document details the **Virtual Anchoring & Pruning** compiler design pattern used in the Arandu Mid-level Intermediate Representation (AMIR) to enable flow-sensitive static validations (such as definite initialization and intraprocedural move checking) on an SSA CFG, while generating optimal register-promoted machine code.

## Visão Geral e Contexto

Virtual anchoring preserva identidade de variáveis para análises de ownership
durante a construção OSSA e remove os anchors antes do backend.

## Detalhes Técnicos da Implementação

### The Architectural Challenge

Modern compilers compile to Static Single Assignment (SSA) form to enable efficient optimizations (like global value numbering, copy propagation, and register allocation). However, SSA by definition eliminates physical variables and represents dataflow as pure values.

In systems languages like Rust, Swift, and Arandu, the compiler must perform safety checks:
1. **Definite Initialization**: Ensuring every variable is initialized before read.
2. **Move/Ownership Checking**: Ensuring a non-copy value is not used after it has been moved.

These checks are naturally **variable-centric** and flow-sensitive: they inspect "when is variable `x` read, written, or moved". If variables are promoted to pure SSA values immediately during AST/HIR lowering:
- All writes/reads of local variables disappear (they become SSA assignments and operands).
- The validation passes cannot easily identify variable boundaries, lifetimes, or spans.

### Proposed Solution: Virtual Anchoring

Arandu resolves this contradiction by introducing temporary **semantic anchors** inside the CFG during AST-to-AMIR lowering. These anchors preserve the variable-level operations for the validators and are subsequently stripped (pruned) before machine code generation.

### 1. AST/HIR Lowering Stage

During lowering, we use an SSA Builder (following Braun et al.'s algorithm) to build pure SSA values.
- **Variable Writes (`write_variable_source`)**:
  - Updates the SSA builder's `current_def` map with the new value.
  - Emits a virtual **Store** statement:
    `Store(local, value)`
- **Variable Reads (`read_variable_source`)**:
  - Queries `current_def` to retrieve the active SSA value (`val`).
  - Emits a virtual **Load** statement assigning to a fresh virtual temporary `temp`:
    `temp = Load(local)`
  - Registers the redirection: `redirected_temps.insert(temp, val)`.
  - The read expression returns `temp`.

### 2. Validation Stage

The checkers (`check_definite_init` and `check_moves`) run on the unoptimized AMIR:
- The move checker traces moves and re-initializations by inspecting the physical `Store` and `Load` statements.
- Variable spans and names are preserved because the virtual instructions reference the `LocalId` directly.

### 3. Rewriting & Pruning Stage

Once all validation checks pass successfully, the compiler optimizes the AMIR:
1. **Redirection Resolution**: `rewrite_all_operands()` recursively replaces all uses of virtual temporaries (`temp`) with their final, resolved SSA values from the redirection map.
2. **Pruning**: `prune_dummy_loads_stores()` sweeps the CFG and deletes all simple `Store` and `Load` statements (those with empty projections).

### CFG Transformation Example

#### Source Code:
```swift
func add(a: int, b: int) -> int {
    let mut x = a
    x = x + b
    return x
}
```

#### Raw AMIR (with Virtual Anchors):
```text
bb0:
  s0 = _1              // Store (x, a)
  _3 = Load(s0)        // Load virtual temp for x
  _4 = add _3, _2      // add x, b
  s0 = _4              // Store (x, add_result)
  _5 = Load(s0)        // Load virtual temp for x
  _0 = _5              // return_val = x
  return
```

#### Optimized & Pruned AMIR:
```text
bb0:
  _4 = add _1, _2
  _0 = _4
  return
```

## PONTOS DE MELHORIA (O que não está no roadmap)

Toda nova forma de projeção ou terminador deve participar do anchoring,
rewriting e pruning; divergência entre visitors pode produzir validação
incorreta mesmo quando o backend ainda gera código.

## Futuro e Próximos Passos

Ampliar somente com testes de CFG adversarial, dominância, move/liveness e
paridade dos backends.
