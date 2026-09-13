# Conformidade de Segurança e Mapeamento de CWEs — Compilador Arandu

## 1. Visão Geral

Este documento formaliza as garantias de segurança de memória, concorrência, integridade de tipos, robustez do frontend e proteção da cadeia de compilação do **Compilador Arandu** contra as vulnerabilidades críticas catalogadas pelo **Common Weakness Enumeration (CWE)** e pelas recomendações do **SEI CERT Coding Standard**, alinhado ao modelo de qualidade de software **ISO/IEC 25010**.

O compilador Arandu foi projetado sob o princípio de **segurança por construção**: comportamentos indefinidos (*Undefined Behavior — UB*) e corrupção de memória são eliminados na fase de análise semântica estática, sem incorrer em custo de execução (*zero-cost abstractions*). Onde a verificação estática pura não for decidível em tempo de compilação (ex: índices de vetores dinâmicos), o compilador e o runtime emitem verificações de limites defensivas (*bounds checks*) com armadilhas determinísticas imediatas (*hardware traps/panics*).

---

## 2. Matriz de Conformidade CWE — Linguagem e Tempo de Execução

### Pilar I: Segurança de Memória e Acesso a Buffers

| CWE ID | Nome da Fraqueza | Classificação no C/C++ | Garantia Arquitetural do Arandu | Mecanismo Formal do Compilador |
|---|---|---|---|---|
| **CWE-416** | *Use After Free* | Crítica (RCE / Crash) | **Eliminada Estaticamente** | **OSSA Move Checker (`O001`, `O005`)**: invalida a variável no CFG após transferência de ownership. Em arenas dinâmicas, o **GenRef** invalida a geração do slot no momento do *retirement*, prevenindo acesso estragado sem problema ABA. |
| **CWE-415** | *Double Free* | Crítica (Memory Corruption) | **Eliminada Estaticamente** | **Linearidade de Tipos + Cascade Drop Glue**: o compilador só emite `AmirStmt::Destroy` para variáveis em estado `MoveState::Live`. Variáveis movidas não sofrem drop duplicado. |
| **CWE-476** | *NULL Pointer Dereference* | Alta (Crash / DoS) | **Eliminada por Tipagem** | **Ponteiros Nulos Ausentes na Superfície Segura**: referências `ref T` e `mut ref T` são não-nulas por contrato estático. Ausência de valor é representada exclusivamente pelo enum discriminado `Option<T>`. |
| **CWE-825** | *Expired Pointer Dereference* | Alta (Escapamento de Pilha) | **Eliminada por Análise de Escape** | **Borrow Checker (`O010`, `O004`)**: empréstimos locais possuem janelas de vida (*loan windows*) restritas ao escopo léxico da raiz. Tentativas de retornar referências locais emitem `O010` determinístico. |
| **CWE-787** | *Out-of-bounds Write* | Crítica (Buffer Overflow) | **Eliminada por Bounds Checking e Contratos** | **Hardware Traps em Runtime + Coleções Seguras**: mutações indexadas em arrays e fatias emitem verificação estrita de limites via comparação unsigned no backend Cranelift (`trapnz` se `idx >= len`), impedindo escrita fora do buffer. |
| **CWE-125** | *Out-of-bounds Read* | Crítica (Heartbleed / Infoleak) | **Eliminada por Bounds Checking e Acesso Seguro** | **Hardware Traps em Runtime + `Slice.get`**: leituras indexadas diretas emitem checagem unsigned de limites no codegen Cranelift (`trapnz` se `idx >= len` ou `idx < 0`), enquanto acessos funcionais retornam `Option<ref T>`. |
| **CWE-119** / **CWE-120** | *Improper Buffer Restriction / Copy* | Crítica (Memory Corruption) | **Eliminada por Tipagem de Buffers** | Strings (`str`) e fatias (`[]T`) são pares indissociáveis de ponteiro e tamanho `(ptr, len)`. Não existem funções no estilo `strcpy`/`gets` que dependam de terminação nula (`\0`). |
| **CWE-129** | *Improper Validation of Array Index* | Média/Alta (OOB Access) | **Eliminada por Tipagem e Verificação Unsigned** | O type checker exige índice inteiro (`T017`). Em codegen Cranelift, índices inteiros com sinal são estendidos com sinal (`sextend`) para a largura do ponteiro nativo (`ptr_type`), e uma única instrução de comparação sem sinal (`IntCC::UnsignedGreaterThanOrEqual`) valida simultaneamente limites superiores e rejeita índices negativos (números negativos tornam-se inteiros $> 2^{63}$, disparando imediatamente o trap). |
| **CWE-843** / **CWE-704** | *Type Confusion / Incorrect Cast* | Crítica (Corrupção de Tipos) | **Eliminada na Superfície Segura** | Uniões em Arandu são enums discriminados com tag e payload protegidos e checagem de exaustividade (`T024`). Casts de ponteiros arbitrários são proibidos em código seguro e restritos a blocos `unsafe` (`O012`, `O013`, `O014`). |
| **CWE-590** / **CWE-763** | *Free of Non-Heap Memory / Invalid Pointer* | Alta (Memory Corruption) | **Eliminada por Construção** | Não há primitiva de `free()` manual em código seguro. A desalocação ocorre exclusivamente via RAII coordenado pelo compilador. |

---

### Pilar II: Aritmética, Literais e Lógica Numérica

| CWE ID | Nome da Fraqueza | Classificação no C/C++ | Garantia Arquitetural do Arandu | Mecanismo Formal do Compilador |
|---|---|---|---|---|
| **CWE-190** | *Integer Overflow* | Média/Alta (Wrap Imprevisto) | **Mitigada por Aritmética Checked** | **Checked Arithmetic por Contrato**: literais estáticos são validados em compilação (`T038`), negação unsigned e div/mod por zero são rejeitados estaticamente (`T005`, `T040`), e a execução em C/Cranelift anula UB via `-fwrapv` e runtime traps (RFC Semântica de Operadores v0.1). |
| **CWE-191** | *Integer Underflow (Wraparound Negativo)* | Média/Alta (Wrap Imprevisto) | **Mitigada por Aritmética Checked** | Negação de inteiros sem sinal (`-u`) é rejeitada estaticamente com `T005`. Deslocamentos de bits com valores negativos (`x << -1`) são rejeitados com `T038`. Em runtime, opera sob as mesmas garantias de wrapping determinístico de complemento de dois (`-fwrapv`). |
| **CWE-369** | *Divide by Zero* | Alta (Crash / DoS) | **Eliminada Estaticamente para Literais** | Tentativa de divisão ou resto por zero literal (`/ 0`, `% 0`, `/ 0.0`) é rejeitada em compilação com `T040DivisionByZero`. Divisões dinâmicas geram trap/panic controlado no runtime em vez de comportamento indefinido. |
| **CWE-197** / **CWE-681** | *Numeric Truncation / Cast Loss* | Média (Perda de Precisão) | **Mitigada por Cast Explícito** | Coerções implícitas que causam perda de bits ou precisão são proibidas (`T015ImplicitWidening`). Casts via operador `as` são estritamente explícitos com truncamento determinístico documentado. |
| **CWE-480** / **CWE-481** | *Assigning instead of Comparing* (`if x = 1`) | Média (Lógica Incorreta) | **Eliminada por Sintaxe e Tipagem** | Atribuição (`let x = 1` ou `x = 1`) é um comando (*statement*), não uma expressão com valor. A condição de controle de fluxo (`if`, `while`) exige estritamente tipo `bool` primitivo (`T009`), tornando esse erro impossível em Arandu. |
| **CWE-193** | *Off-by-one Error* | Média (OOB / Loop Bounds) | **Prevenida por Idioma de Sintaxe** | Construções de loop operam sobre iteradores de coleções (`for item in list`) e fatias de intervalo semiaberto `0..n`, prevenindo erros comuns de contadores manuais com `<=`. |

---

### Pilar III: Frontend, Lexer e Integridade de Código-Fonte

| CWE ID | Nome da Fraqueza | Classificação no C/C++ | Garantia Arquitetural do Arandu | Mecanismo Formal do Compilador |
|---|---|---|---|---|
| **CWE-1307** / **CVE-2021-42574** | *Trojan Source / BiDi Text Attack* | Alta (Ofuscação de Código Malicioso) | **Eliminada Estaticamente** | **Detecção de BiDi no Lexer (`LX004BidiTrojanSource`)**: o lexer intercepta e rejeita caracteres de formatação bidirecional Unicode invisíveis (`U+202A`..`U+202E`, `U+2066`..`U+2069`, `U+200E`, `U+200F`, `U+061C`) em comentários, doc-comments, strings literais e código, impedindo que revisores humanos vejam lógica diferente da executada. |
| **CWE-1007** | *Visual Homoglyph Attack* | Média (Confusão de Identificadores) | **Eliminada por Especificação de Identificadores** | O lexer restringe identificadores à especificação rigorosa do **Unicode Standard Annex #31 (UAX #31)** (`unicode_ident::is_xid_start`, `is_xid_continue`), bloqueando caracteres de scripts incompatíveis. |
| **CWE-134** | *Externally-Controlled Format String* | Crítica (RCE em C) | **Eliminada por Tipagem Estática** | Interpolações e macros de formatação são resolvidas estaticamente. Valores que não implementam o contrato de formatação são rejeitados com `T034CannotFormat`. Não há ponteiros de formatação estilo C (`%n`, `%s`). |
| **CWE-754** / **CWE-391** | *Unchecked Error Condition* | Média/Alta (Falha Silenciosa) | **Detectada por Análise de Fluxo** | O compilador emite o diagnóstico `W006UnhandledResult` quando valores retornados do tipo `Result<T, E>` não são inspecionados ou descartados explicitamente. |

---

### Pilar IV: Fluxo de Controle, Exaustividade e Recursão

| CWE ID | Nome da Fraqueza | Classificação no C/C++ | Garantia Arquitetural do Arandu | Mecanismo Formal do Compilador |
|---|---|---|---|---|
| **CWE-457** | *Use of Uninitialized Variable* | Média/Alta (UB) | **Eliminada por Análise de Fluxo** | **Definite Initialization (`O008`)**: rastreamento de *InitBits* através de todas as arestas do grafo de fluxo de controle (CFG), exigindo inicialização em todos os caminhos antes do uso. |
| **CWE-478** | *Missing Default Case in Multiple Condition* | Média/Alta (Comportamento Omitido) | **Eliminada Estaticamente** | **Checagem de Exaustividade (`T024NonExhaustiveMatch`)**: expressões `match` sobre enums são verificadas estaticamente em `match_exhaust.rs`. Todas as variantes devem ser cobertas explicitamente ou por meio de padrão curinga (`_`). |
| **CWE-674** | *Uncontrolled Recursion / Stack Exhaustion* | Alta (DoS / Crash) | **Mitigada Estaticamente no Compilador** | Tipos recursivos sem indireção de ponteiro são rejeitados com `T029RecursiveStructInfiniteSize`. Ciclos e limites de monomorfização de genéricos são contidos por `G001GenericInstantiationCycle` e `G002GenericInstantiationLimit`. |
| **CWE-570** / **CWE-571** / **CWE-670** | *Expression Always False/True / Unreachable Code* | Baixa/Média (Código Inoperante) | **Detectada por Linting e Análise** | Emissão determinística de alertas para código morto (`W002DeadCode`) e instruções inalcançáveis (`W003UnreachableCode`) geradas por condicionais invariantes. |

---

### Pilar V: Concorrência e Gerenciamento de Recursos

| CWE ID | Nome da Fraqueza | Classificação no C/C++ | Garantia Arquitetural do Arandu | Mecanismo Formal do Compilador |
|---|---|---|---|---|
| **CWE-362** | *Race Condition (Data Race)* | Alta (Concorrência Indeterminística)| **Eliminada por Tipagem de Concorrência** | **Transferência Linear em Canais e Locks Cooperativos**: `Channel<T>` transfere ownership do payload no envio; `AsyncMutex<T>` isola acesso mutável exclusivo sob lock. |
| **CWE-772** / **CWE-401** | *Missing Release of Resource / Memory Leak* | Média/Alta (Exaustão de Recursos) | **Eliminada por RAII e Destrutores** | Variáveis que saem de escopo executam a rotina de limpeza linear da AMIR (`AmirStmt::Destroy`). Tipos de usuário com gerenciamento de recursos externos utilizam a anotação `@Destructor` (`T035`). |
| **CWE-833** | *Deadlock* | Alta (Hang / DoS) | **Mitigada por Concorrência Cooperativa** | O modelo padrão de concorrência do Arandu favorece canais sem bloqueio de threads de SO (*lock-free circular channels*) e agendamento cooperativo assíncrono. |

---

## 3. Matriz de Segurança da Ferramenta de Compilação e Cadeia de Suprimentos

Como o compilador e suas ferramentas de linha de comando processam código-fonte de terceiros, manifestos de pacotes e caches de compilação, o próprio compilador implementa defesas contra ataques à ferramenta:

| CWE ID | Nome da Fraqueza | Superfície de Risco | Garantia Arquitetural do Arandu | Mecanismo de Proteção |
|---|---|---|---|---|
| **CWE-22** | *Path Traversal* (Top 8 no Top 25) | Resolução de Módulos e Pacotes | **Eliminada Estaticamente** | **Isolamento de Pacote (`M005FilesystemImportForbidden`)**: em modo pacote, o compilador proíbe imports literais com caminhos de sistema de arquivos arbitrários, confinando toda a resolução ao grafo de módulos do pacote. |
| **CWE-59** | *Improper Link Resolution (Symlink Traversal)* | Descoberta de Arquivos de Projeto | **Eliminada por Isolamento** | O discovery de projetos audita canonicalização de caminhos (`cli_project_adversarial.rs`) e rejeita que raízes de fontes sigam links para fora do pacote. |
| **CWE-78** / **CWE-88** | *OS Command Injection / Argument Injection* (Top 5) | Invocação de Linkers de Sistema | **Eliminada por Arquitetura** | **Process Spawning Tipado**: a invocação do linker do sistema (`gcc`, `ld`) e ferramentas externas é feita via `std::process::Command` passando argumentos estruturados em vetor, sem intermediários de shell (`sh -c` ou `cmd.exe`). |
| **CWE-403** | *Exposure of File Descriptor to Child Processes* | Execução de Subprocessos | **Eliminada por Contrato de Processo** | No Linux/macOS, a invocação de processos externos utiliza descritores com `O_CLOEXEC` ativo por padrão, prevenindo vazamento de handles de arquivos ou sockets. |
| **CWE-732** / **CWE-377** | *Incorrect Permission / Insecure Temp Files* | Compilação AOT e Linkagem | **Eliminada por Isolamento** | Objetos compilados intermediários (`.o`) e bibliotecas estáticas são criados estritamente dentro da pasta `target/` do projeto ou em diretórios de sessão seguros com permissões restritas (0700/0600), prevenindo ataques de symlink e acesso indevido. |
| **CWE-502** | *Deserialization of Untrusted Data* | Caches Incrementais de Compilação | **Mitigada por Hashing Criptográfico** | Dados cacheados entre sessões de compilação incremental validam integridade com digests estáveis de conteúdo (BLAKE3) antes de reidratar AST/HIR ou artefatos de código. |
| **CWE-400** / **CWE-770** | *Uncontrolled Resource Consumption* (Compiler DoS) | Expansão de Genéricos e Caches Salsa | **Mitigada por Guardrails e Memoização** | Limite rígido de recursão em instanciação de genéricos (`G002`), interning estrutural de tipos e early-cutoff das queries Salsa impedem explosão combinatorial de tempo e memória. |

---

## 4. Modelo de Qualidade de Software (ISO/IEC 25010)

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
   - Ausência de *panics* ou *unwraps* descontrolados no compilador perante entradas de usuário inválidas.
4. **Segurança (Security)**:
   - Eliminação de vulnerabilidades críticas do CWE Top 25 em tempo de compilação e tempo de execução.
   - Defesa estrita em tempo de lexing contra ataques de *Trojan Source* (`CWE-1307` via `LX004`).
   - *Bounds checking* determinístico via hardware traps contra *buffer overflows* (`CWE-125`, `CWE-787`, `CWE-129`).
   - Confinamento estrito de código inseguro em blocos explícitos com diagnósticos dedicados (`O012`, `O013`, `O014`).
5. **Manutenibilidade (Maintainability)**:
   - Separação modular rigorosa entre crates definida em `AGENTS.md` e auditada continuamente por `xtask check-architecture`.
   - Isolamento de execução de testes por processo nativo via `cargo-nextest`.
