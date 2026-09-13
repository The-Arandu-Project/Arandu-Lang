# RFC: Semântica de Operadores e Aritmética v0.1

**Status:** Proposed (Under Review)

**Supersedes:** Aritmética implícita e comportamento ad-hoc herdado dos backends

---

## Visão Geral e Contexto

Arandu é uma linguagem de sistemas com foco em segurança por construção, determinismo e compilação incremental. O compilador possui dois backends de produção: **Cranelift** (JIT e AOT nativo) e **C Backend** (AOT portável).

Historicamente, enquanto linguagens legadas (C, C++, JavaScript, PHP) acumularam bizarrices semânticas — como coerções implícitas perigosas, comportamento indefinido (Undefined Behavior — UB) em overflow assinado e shifts fora do range —, linguagens modernas de ponta (Rust, Swift, Zig e Go) estabeleceram padrões rigorosos para eliminar classes inteiras de falhas de segurança (como CWE-190: *Integer Overflow* e CWE-787: *Buffer Overflow*).

No Arandu, embora o sistema de tipos já previna a maior parte das coerções fracas clássicas, a auditoria arquitetural revelou lacunas críticas:
1. **Negação de Unsigned:** O type checker permite `-x` para tipos unsigned (`u8..u64`, `uint`), gerando complementos de dois modulares inesperados e valores negativos internados no otimizador.
2. **Operadores Relacionais sem Ordem:** `<, <=, >, >=` exigem apenas unificação de tipos, permitindo comparações em `struct`, `tuple`, `bool` e `str` — causando *Internal Compiler Errors* (ICE) no Cranelift e erros de compilação C inválidos.
3. **Ponteiros com Comparação Assinada:** O backend Cranelift compara ponteiros com `SignedLessThan`, considerando endereços de memória alta como negativos.
4. **Discrepância entre Otimizador (SCCP) e Runtime:** O SCCP dobra constantes inteiras em `i128` sem truncamento para a largura do tipo, fazendo com que `255_u8 + 1` dobre para `256`, enquanto em runtime envolveria para `0`, invertendo o fluxo de controle em branch conditions.
5. **Contrato de Overflow e Resto:** `docs/standards/CWE-COMPLIANCE.md` alega verificação de capacidade para CWE-190, mas os backends operam com wrapping desprotegido (Cranelift) ou herdam UB do C padrão (`+` assinado). Além disso, `%` em float é aceito no typeck, compilando via `fmod` no Cranelift mas falhando sintaticamente no backend C.

Este RFC formaliza o **Contrato de Operadores e Aritmética do Arandu v0.1**, adotando os acertos do mercado e eliminando todas as expressões malditas e fontes de divergência entre backends.

---

## Detalhes Técnicos da Implementação

### 1. Matriz de Operadores e Classificação de Tipos

O sistema de tipos classifica os tipos em categorias operacionais estritas:

| Categoria | Tipos Integrantes |
| :--- | :--- |
| **Inteiro com Sinal (`SignedInt`)** | `i8`, `i16`, `i32`, `i64`, `int` |
| **Inteiro sem Sinal (`UnsignedInt`)** | `u8`, `u16`, `u32`, `u64`, `uint`, `byte` |
| **Ponto Flutuante (`Float`)** | `f32`, `f64`, `float` |
| **Escalar de Caractere (`Char`)** | `char` (Unicode Scalar Value `0x0000..=0x10FFFF`) |
| **Booleano (`Bool`)** | `bool` (`true`, `false`) |
| **Ponteiro Cru (`RawPtr`)** | `ptr[T]` |
| **Compostos (`Composite`)** | `struct`, `tuple`, `enum`, `array`, `slice`, `str` |

#### Matriz de Aplicação de Operadores:

| Operador | Categorias Válidas | Comportamento Contratual |
| :--- | :--- | :--- |
| **`+`, `-`, `*`** | `SignedInt`, `UnsignedInt`, `Float` | Ambos os operandos devem unificar o mesmo tipo. Overflow em inteiros segue a política do item 2. Floats seguem IEEE 754-2019. |
| **`\`** | `SignedInt`, `UnsignedInt`, `Float` | Divisão inteira truncada em direção ao zero. Divisão por literal `0` é **erro estático** (`T040`). Divisão por zero dinâmica gera **trap determinístico**. Floats produzem $\pm\infty$ ou NaN. |
| **`%`** | `SignedInt`, `UnsignedInt` | **Estritamente restrito a inteiros**. Resto truncado ($sign(a \% b) = sign(a)$). Divisor `0` gera erro estático ou trap. Para floats, é obrigatório chamar função padrão (`math.mod`). |
| **`-` (unário)** | `SignedInt`, `Float` | **Proibido em `UnsignedInt`** (`T005OperatorNotApplicable`). |
| **`!`, `&&`, `\|\|`** | `Bool` | Operações lógicas puras. Curto-circuito garantido em `&&` e `\|\|`. |
| **`~`, `&`, `\|`, `^`** | `SignedInt`, `UnsignedInt` | Bitwise bit a bit. |
| **`<<`, `>>`** | `SignedInt`, `UnsignedInt` | O operando direito deve ser inteiro positivo. Shift $\ge$ bit width literal é **erro estático**. Em runtime, segue semântica determinística sem UB (item 3). |
| **`==`, `!=`** | Escalares (`Int`, `Float`, `Char`, `Bool`, `RawPtr`), `str`, `Option`/`Nullable` com `nil` | Comparação por valor para escalares; comparação de conteúdo para `str` (`ar_str_eq`). **Proibido em structs e tuplas nativas** sem interface explícita (`std.core.cmp.PartialEq`). Proibida comparação de identidade referencial disfarçada de valor. |
| **`<`, `<=`, `>`, `>=`** | `SignedInt`, `UnsignedInt`, `Float`, `Char` | **Restrito a tipos matematicamente ordenáveis (`Orderable`)**. Proibido em `bool`, `struct`, `tuple`, `str` (salvo via método `cmp`) e `ptr` direto. |

---

### 2. Contrato de Overflow de Inteiros (CWE-190)

Inspirado nas decisões do **Swift** e **Zig**:
1. **Comportamento Padrão de Execução:** Operações aritméticas com estouro de capacidade em runtime disparam **trap determinístico de segurança** (ou abort seguro com código de saída padronizado), garantindo a promessa de `CWE-COMPLIANCE.md`.
2. **Aritmética Modular Explícita:** Se o programador necessita deliberadamente de aritmética modular com wrap (e.g. criptografia, hashing FNV/Murmur), deve utilizar métodos explícitos da biblioteca padrão (ex: `wrapping_add`, `wrapping_sub`, `wrapping_mul`) ou operadores modulares dedicados caso venham a ser introduzidos.
3. **Paridade C ↔ Cranelift:**
   * No **Cranelift**: Emissão com verificação de overflow (usando instruções condicionadas a overflow flags ou `iadd_ifcout` / checks antes de instrução).
   * No **Backend C**: O código C gerado compila obrigatoriamente com o sinalizador `-fwrapv` (para anular UB de signed overflow no GCC/Clang) e, para builds checked, utiliza as funções intrínsecas seguras `__builtin_add_overflow`, `__builtin_sub_overflow`, `__builtin_mul_overflow`.
4. **Constantes no Compilador:** O estouro estático em expressões literais (ex: `127_i8 + 1`) gera erro de compilação (`T038IntegerLiteralOutOfRange`).

---

### 3. Contrato de Bitwise Shifts (`<<`, `>>`)

Inspirado na robustez de **Go** e **Swift**:
1. **Verificação Estática:** Se o operando direito de um shift for uma constante literal tal que $shift < 0$ ou $shift \ge bit\_width(lhs)$, o compilador rejeita com erro estático.
2. **Semântica de Runtime:** Para valores dinâmicos:
   * Se $shift \ge bit\_width(lhs)$: o resultado avalia deterministamente para `0` para tipos unsigned e tipos positivos, ou `-1`/`0` para signed de acordo com o sinal, eliminando vazamento de comportamento de registradores x86/ARM e eliminando UB do backend C.
   * Não ocorre truncamento mascarado acidental dependente de arquitetura de CPU.

---

### 4. Contrato de Comparações e Igualdade

1. **Ordenabilidade (`Orderable`):**
   * Tipos primitivos numéricos (`int`, `uint`, `float`) e `char` são intrinsecamente ordenáveis.
   * `bool` **não** é ordenável (`true < false` rejeitado com `T005`).
   * `str` não permite `<` nativo diretamente na sintaxe v0.1 para evitar ambiguidades entre ordenação por bytes UTF-8 e ordenação alfabética localizada (collation). Comparações de string devem usar `std.core.cmp` ou método explícito `.compare()`.
2. **Ponteiros crus (`ptr[T]`):**
   * Comparação de igualdade (`==`, `!=`) é permitida para verificar nulidade ou mesmice de endereço.
   * Comparações relacionais (`<`, `<=`, `>`, `>=`) em ponteiros, quando necessárias em código `unsafe`, **devem** ser tratadas como inteiros unsigned (`IntCC::UnsignedLessThan` no Cranelift e `uintptr_t` no backend C), eliminando a falha de considerar ponteiros em endereços altos como números negativos.
3. **Structs e Tuplas:**
   * Proibido o uso de `==`, `!=`, `<`, `>` diretamente em instâncias de structs e tuplas sem implementação comprovada da interface `PartialEq` ou `Ord` da stdlib.
   * Fica terminantemente proibido o Cranelift comparar endereços de ponteiros sob a fachada de um `==` entre structs.

---

### 5. Contrato do Otimizador AMIR (SCCP e GVN)

1. **Folding Tipado:** O otimizador de propagação condicional de constantes (SCCP) nunca deve avaliar operações inteiras em um espaço ilimitado `i128` sem truncar o resultado para a representação do tipo do operando.
2. **Paridade com Runtime:** O resultado de uma expressão dobrada em tempo de compilação deve ser bit-a-bit idêntico ao resultado gerado pelo runtime em modo desotimizado (`OptLevel::O0`).
3. **Preservação de IEEE 754:** Mantém-se o contrato já testado de que `NaN == NaN` nunca dobra para `true`.

---

## PONTOS DE MELHORIA (O que não está no roadmap)

- Sobrecarga arbitrária de operadores definidos pelo usuário através de métodos mágicos (estilo Python `__add__` ou C++ `operator+`) permanece fora do escopo do Arandu v0.1.
- Operadores saturantes dedicados (`+|`, `-|` no estilo Zig) permanecem adiados para avaliação futura pós-v0.1.
- Comparações com tolerância epsilon em floats (`approx_eq`) pertencem à biblioteca padrão (`std.math`), nunca à sintaxe `==`.

---

## Futuro e Próximos Passos

1. Aprovação do plano de implementação e deste RFC.
2. Endurecimento do Type Checker (`arandu_typeck`).
3. Refatoração do SCCP (`arandu_mir`).
4. Alinhamento de codegen e flags nos backends (`arandu_backend_cranelift` e `arandu_backend_c`).
5. Criação da suíte unificada de testes de borda (`semantic_edge_cases.rs`) e testes de paridade de execução dupla.
