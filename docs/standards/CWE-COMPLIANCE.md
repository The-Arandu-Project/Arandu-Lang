# Conformidade de Segurança e Mapeamento de CWEs — Compilador Arandu

## 1. Visão Geral

Este documento formaliza as garantias de segurança de memória, concorrência e tipos do **Compilador Arandu** contra as vulnerabilidades críticas catalogadas pelo **Common Weakness Enumeration (CWE)** e pelas recomendações do **SEI CERT Coding Standard**, alinhado ao modelo de qualidade de software **ISO/IEC 25010**.

O compilador Arandu foi projetado sob o princípio de **segurança por construção**: comportamentos indefinidos (Undefined Behavior — UB) e vulnerabilidades de memória são eliminados na fase de análise semântica estática, sem incorrer em custo de execução (zero-cost abstractions).

---

## 2. Matriz de Conformidade CWE

| CWE ID | Nome da Fraqueza | Classificação no C/C++ | Garantia Arquitetural do Arandu | Mecanismo Formal do Compilador |
|---|---|---|---|---|
| **CWE-416** | *Use After Free* | Crítica (RCE / Crash) | **Eliminada Estaticamente** | **OSSA Move Checker (`O001`, `O005`)** invalida a variável no CFG após transferência de ownership. Em arenas dinâmicas, o **GenRef** invalida a geração do slot no momento do *retirement*, prevenindo acesso estragado sem ABA. |
| **CWE-415** | *Double Free* | Crítica (Memory Corruption) | **Eliminada Estaticamente** | **Linearidade de Tipos + Cascade Drop Glue**: o compilador só emite `AmirStmt::Destroy` para variáveis em estado `MoveState::Live`. Variáveis movidas não sofrem drop duplicado. |
| **CWE-476** | *NULL Pointer Dereference* | Alta (Crash / DoS) | **Eliminada por Tipagem** | **Ponteiros Nulos Ausentes na Superfície Segura**: referências `ref T` e `mut ref T` são não-nulas por contrato estático. Ausência de valor é representada exclusivamente pelo enum discriminado `Option<T>`. |
| **CWE-825** | *Expired Pointer Dereference* | Alta (Escapamento de Pilha) | **Eliminada por Análise de Escape** | **Borrow Checker (`O010`, `O004`)**: empréstimos locais possuem janelas de vida (*loan windows*) restritas ao escopo léxico da raiz. Tentativas de retornar referências locais emitem `O010` determinístico. |
| **CWE-457** | *Use of Uninitialized Variable* | Média/Alta (UB) | **Eliminada por Análise de Fluxo** | **Definite Initialization (`O008`)**: rastreamento de *InitBits* através de todas as arestas do grafo de fluxo de controle (CFG), exigindo inicialização em todos os caminhos antes do uso. |
| **CWE-190** | *Integer Overflow* | Média/Alta (Wrap Imprevisto) | **Mitigada por Aritmética Checked** | **Checked Arithmetic por Padrão**: operações aritméticas verificam transbordamento de capacidade baseado na largura do target (`TargetInfo`), evitando wraps silenciosos. |
| **CWE-362** | *Race Condition (Data Race)* | Alta (Concorrência Indeterminística)| **Eliminada por Tipagem de Concorrência** | **Transferência Linear em Canais e Locks Cooperativos**: `Channel<T>` transfere ownership do payload no envio; `AsyncMutex<T>` isola acesso mutável exclusivo sob lock. |
| **CWE-787** | *Out-of-bounds Write* | Crítica (Buffer Overflow) | **Eliminada por Contratos Seguros** | **Borrowed Views (`[]T`) e Coleções Seguras**: `Slice.get` e coleções como `SmallVec` realizam checagem de limites e retornam `Option<ref T>`, impedindo corrupção de memória. |

---

## 3. Modelo de Qualidade de Software (ISO/IEC 25010)

O compilador Arandu implementa as características da norma **ISO/IEC 25010**:

1. **Adequação Funcional (Functional Suitability)**:
   - Cobertura de testes unitários e de integração com bijeção estrita de diagnósticos (`xtask check-diag-docs`).
   - Pipeline de testes determinístico com suítes de golden fixtures para AST, HIR e AMIR.
2. **Eficiência de Desempenho (Performance Efficiency)**:
   - Estruturas de dados Data-Oriented Design (DoD): `AstPool`, `HirPool`, `IndexVec` e inteiros estáveis em vez de ponteiros esparsos.
   - Zero heap allocations em regime permanente para coleções híbridas (`SmallVec4` na pilha) e concorrência (`Channel` circular).
   - Invalidação incremental $O(1)$ via Salsa Queries puras (`arandu_query`).
3. **Confiabilidade (Reliability)**:
   - Recuperação em cascata com nós de erro na árvore sintática concreta (Rowan CST), impedindo que erros do usuário causem interrupção do compilador (*Internal Compiler Error — ICE*).
4. **Manutenibilidade (Maintainability)**:
   - Separação modular rigorosa entre crates definida em `AGENTS.md` e auditada continuamente por `xtask check-architecture`.
   - Isolamento de execução de testes por processo nativo via `cargo-nextest`.
