# RFC 0013: Metaprogramação Determinística em Tempo de Compilação (CTFE & Comptime) via AMIR VM

- **Número da RFC:** 0013
- **Título:** Metaprogramação Determinística em Tempo de Compilação (CTFE & Comptime) via AMIR VM
- **Autor(es):** Bruno e Equipe do Compilador Arandu
- **Data de Início:** 2026-09-12
- **Status:** `Draft`
- **Área Principal:** `Frontend` / `Middle-end` / `Runtime`
- **PR da RFC:** N/A (In-Tree RFC)
- **Issue de Acompanhamento:** N/A

---

## 1. Resumo (Summary)

Esta RFC define a arquitetura formal, as extensões de sintaxe, o modelo de memória virtual e o contrato de integração com o motor incremental Salsa para o sistema de **Metaprogramação em Tempo de Compilação e Execução de Funções em Compile-Time (CTFE)** do Arandu.

A proposta elimina a necessidade histórica de linguagens de macro separadas (como o `macro_rules!` do Rust) e o peso de bibliotecas procedurais externas compiladas dinamicamente (`proc-macros`), adotando a filosofia de **"Mesma Linguagem, Sem Macros Secundárias"** inspirada no **Zig (`comptime`)**, combinada com a segurança e precisão semântica do **Miri (Rust)** sobre representações intermediárias.

Os pilares deste design são:
1. **Unificação Sintática (`comptime`)**: Expressões, parâmetros, blocos e condicionais executam código Arandu padrão em tempo de compilação sem dialetos paralelos.
2. **Reflexão Estática de Tipos (`std.core.meta`)**: Introspecção completa de tipos, campos, métodos e anotações em tempo de compilação, eliminando 90% dos casos de uso de macros tradicionais (derivação de serialização, debug, clone, hash e igualdade) através de laços desdobrados (`comptime for`) e acesso indexado a campos (`val.@field(name)`).
3. **Quasiquoting Higiênico (`quote { ... }`)**: Mecanismo de citação e injeção de código tipado com splicing `${expr}` para os 10% restantes de metaprogramação (geração de novas estruturas, interfaces e DSLs embutidas).
4. **Execução Segura em AMIR VM (Estilo Miri)**: O interpretador de compile-time opera sobre o grafo SSA/OSSA da **AMIR**, com modelo de memória virtual rastreado, checagem estrita de bounds e detecção precoce de *use-after-free*, alinhado à ABI exata do alvo de compilação (`TargetInfo`).
5. **Integração Salsa-First & Imunidade a Travamentos no LSP**: Cada execução de comptime é uma query pura e memoizada com *early-cutoff*. A execução roda sob um orçamento estrito de passos (*fuel budget*) com cancelamento cooperativo assíncrono, garantindo que código incompleto ou laços infinitos digitados no editor jamais travem a thread do servidor LSP.

---

## 2. Motivação (Motivation)

A metaprogramação em linguagens modernas sofre de três vícios arquiteturais consolidados:

1. **O Modelo Dual do Rust (`macro_rules!` e `proc-macro`)**:
   - **Dupla sintaxe**: Obriga o desenvolvedor a aprender uma linguagem de casamento de padrões sintáticos completamente diferente (`$($x:expr),*`) para macros declarativas.
   - **Overhead severo de compilação**: Procedural macros exigem crates dedicados (`proc-macro = true`), que precisam ser compilados em código de máquina para o host, linkados em bibliotecas dinâmicas (`.so`/`.dylib`/`.dll`) e carregados via `dlopen`. Bibliotecas utilitárias de parsing de tokens (como `syn` e `quote`) frequentemente respondem por uma fração massiva do tempo total de compilação de projetos Rust.
   - **Cegueira de Tipos**: Procedural macros executam antes da resolução de nomes e antes do type checking, operando sobre uma sequência cega de tokens (`TokenStream`). Elas não conseguem saber o tipo real de um campo ou se um tipo implementa uma interface sem que o usuário forneça dicas redundantes.
2. **O Modelo de Macro de Texto em C/C++ (`#define`)**:
   - Falta absoluta de higiene léxica, ausência de segurança de tipos, poluição global de escopos e depuração caótica.
3. **As Invalidações em Cascata do Zig Comptime**:
   - O modelo do Zig é brilhante em ergonomia, mas sua execução de comptime sobre árvores sintáticas cedo demais causa dores de cabeça para compiladores incrementais: uma pequena alteração em uma função comptime frequentemente causa invalidação em cascata em grande escala por falta de um grafo de dependências estrito com early-cutoff.

O Arandu resolve esses problemas combinando seu sistema de queries Salsa, seu backend incremental e o layout de dados denso da AMIR:
- Metaprogramação usa a **própria linguagem Arandu**.
- Não há crates separados de macro nem compilação de bibliotecas dinâmicas no host.
- A reflexão ocorre com **tipos completos conhecidos**.
- A execução na **AMIR VM** garante que o código execute exatamente as mesmas instruções lógicas e regras de ABI do binário compilado.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

### 3.1. A Palavra-Chave `comptime`

A palavra-chave `comptime` instrui o compilador Arandu a avaliar uma expressão, bloco ou parâmetro durante o pipeline de compilação:

```arandu
// 1. Expressão avaliada em tempo de compilação:
let max_buffer: usize = comptime calculateOptimalBufferSize()

// 2. Bloco comptime com asserções estáticas:
comptime {
    let size = @sizeOf(Particle)
    assert(size <= 64, "Particle excede uma linha de cache L1!")
}

// 3. Condicional estática (código do branch falso é fisicamente descartado do AST/AMIR):
comptime if (target.arch == .X86_64) {
    applyAvx2Optimization()
} else {
    applyGenericScalarFallback()
}
```

---

### 3.2. Parâmetros Comptime & Const Generics

`comptime` é o mecanismo canônico para fornecer parâmetros conhecidos estaticamente para funções e tipos, unificando genéricos de tipos e genéricos de constantes:

```arandu
// Matriz com dimensões na stack conhecidas estaticamente (RFC 0012):
struct StaticMatrix<T, comptime M: usize, comptime N: usize> {
    data: [T; M * N]
}

// Função especializada por parâmetros de compilação:
func unrolledMultiply<comptime FACTOR: i32>(value: i32): i32 {
    comptime if (FACTOR == 0) {
        0
    } else comptime if (FACTOR == 1) {
        value
    } else comptime if (FACTOR == 2) {
        value + value
    } else {
        value * FACTOR
    }
}
```

---

### 3.3. Reflexão Estática de Tipos (`std.core.meta`)

Em vez de gerar código de texto através de macros para derivar interfaces comuns, o Arandu expõe o módulo puro `std.core.meta`. O compilador fornece a função intrínseca `@typeInfo(T)` que devolve uma estrutura de reflexão estática completa:

```arandu
import std.core.meta as meta

struct User {
    id: i64,
    name: String,
    active: bool,
}

// Exemplo: Serialização genérica sem NENHUMA macro!
func serializeJson<T>(val: &T, writer: &mut JsonWriter): Result<(), Error> {
    writer.beginObject()?

    // comptime for desdobra o laço em tempo de compilação para cada campo:
    comptime for field in meta.TypeInfo::of::<T>().fields() {
        // Acesso a campos por identificador dinâmico em tempo de compilação:
        let field_value = val.@field(field.name)
        writer.writeField(field.name, field_value)?
    }

    writer.endObject()
}
```

#### O que é eliminado com essa abordagem:
- Elimina-se a macro `#[derive(Serialize)]`.
- Elimina-se a macro `#[derive(Debug)]`.
- Elimina-se a macro `#[derive(PartialEq, Eq, Hash)]`.
- Elimina-se a macro `#[derive(Clone)]`.

Tudo isso se torna código Arandu genérico e transparente, com desdobramento estático de laços e zero overhead em tempo de execução.

---

### 3.4. Quasiquoting Higiênico para Injeção de Código (`quote { ... }`)

Quando uma biblioteca precisa **declarar novos itens** no escopo (como implementar uma interface formal ou gerar novas structs), ela utiliza blocos `quote`:

```arandu
import std.core.meta as meta

// Anotação customizada de derivação:
@meta.DeriveHandler
comptime func deriveToString(target: meta.TypeInfo): meta.Code {
    quote {
        impl Display for ${target.name} {
            func toString(&self): String {
                let mut buf = String::new()
                buf.pushStr(${target.name.literal()})
                buf.pushStr(" { ")
                comptime for field in ${target}.fields() {
                    buf.pushStr(field.name)
                    buf.pushStr(": ")
                    buf.pushStr(self.@field(field.name).toString())
                    buf.pushStr(", ")
                }
                buf.pushStr("}")
                buf
            }
        }
    }
}
```

Uso pelo desenvolvedor:
```arandu
@Derive(ToString)
struct Point {
    x: f32,
    y: f32,
}
```

#### Regras de Higiene de Código:
1. **Identificadores Locais**: Variáveis criadas dentro do bloco `quote` recebem `SymbolId`s novos e isolados, impedindo conflito acidental de nomes com o escopo do usuário (*variable capture*).
2. **Splicing Tipado**: `${expr}` insere expressões da AST ou identificadores avaliados na fase comptime, usando a mesma sintaxe de interpolação já familiar aos desenvolvedores Arandu.

---

### 3.5. Inclusão Determinística de Recursos Externos via Salsa

Para embutir arquivos estáticos (shaders, schemas JSON, imagens ou certificados) no binário em tempo de compilação, o Arandu fornece primitivas que respeitam o grafo Salsa:

```arandu
// Carrega arquivo como fatia imutável de bytes em compile-time:
const SHADER_BYTES: []u8 = comptime meta.embedBytes("shaders/vertex.spv")

// Carrega arquivo como string UTF-8 validada estaticamente:
const CONFIG_SCHEMA: String = comptime meta.embedString("schemas/config.json")
```

> [!IMPORTANT]
> **Invariante de I/O Salsa**: A função `meta.embedBytes(path)` **não** executa uma chamada crua a `std::fs::read` no compilador. Ela registra o arquivo no motor Salsa como um input tracked (`FileId` / `InputBlob`). Se o arquivo externo for modificado no disco, o Salsa invalida cirurgicamente apenas as queries dependentes. Se o conteúdo for idêntico, o *early-cutoff* suprime qualquer recompilação!

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

### 4.1. Arquitetura da AMIR VM (O Interpretador CTFE)

A execução em tempo de compilação não ocorre na AST crua, mas sobre o grafo SSA da **AMIR** (Fase 3 do compilador):

```text
Código Fonte (AST)
       │
       ▼
Resolução de Nomes & Typeck Inicial
       │
       ▼
Lowering para AMIR (SSA, CFG, Places, Borrow Info)
       │
       ▼
AMIR VM (Interpretador de Bytecode Seguro)
 ├── Virtual Memory Engine (Slots tipados, Bounds Check, Miri Model)
 ├── TargetInfo / DataLayout (Tamanho de ponteiro e padding do alvo)
 ├── Fuel Counter (Orçamento de passos contra loops infinitos)
 └── Salsa Memoization Cache
       │
       ▼
ConstValue / AST Injected Code
```

#### Características da AMIR VM:
- **Tabela de Instruções Linear**: Interpreta diretamente as estruturas densas `AmirStmtTable` e `DenseRange` de **A5**, alcançando máxima localidade de cache L1.
- **Memória Virtual Alocada em Arena**: A VM gerencia um espaço de endereçamento virtual isolado. Cada bloco alocado por `comptime` carrega um `AllocId` e uma geração. Acesso fora de limites (*out-of-bounds*) ou desreferenciamento de ponteiro pendente (*dangling pointer*) em tempo de compilação gera um diagnóstico imediato e estruturado (`ICE` ou erro de tipo `Txxx`), em vez de causar *Segmentation Fault* no compilador.
- **Consciência Estrita do Alvo (Target-Awareness)**: Se o compilador estiver rodando em Linux x86_64 compilando para um microcontrolador ARM Cortex-M0 de 32 bits (little-endian), a VM calcula `@sizeOf`, alinhamento e offsets de structs conforme a ABI do alvo de 32 bits.

---

### 4.2. Integração com Salsa & Resiliência do LSP

Toda avaliação de comptime é expressa como uma query pura no banco de dados Salsa:

```rust
#[salsa::query(eval_comptime_query)]
fn eval_comptime(
    db: &dyn SourceDatabase,
    target_func: AmirFuncId,
    args: Vec<ConstValue>,
) -> Result<Arc<ConstValue>, ComptimeError>;
```

#### 1. Early-Cutoff Automático
Se o desenvolvedor alterar a implementação de uma função utilitária `comptime`, mas a saída para uma determinada chamada for idêntica (ex: `ConstValue::U64(1024)`), o Salsa corta a propagação da invalidação (*early-cutoff*), impedindo a re-emissão de código nos backends e preservando a reatividade instantânea.

#### 2. Proteção do Servidor LSP (Fuel Budget)
Para evitar que erros de digitação comuns (como laços `while (true)` não intencionais) congelem o IDE:
- Cada invocação de query recebe um orçamento estrito de passos (*fuel*):
  - **No CLI (Build normal)**: Padrão de `1.000.000` de passos de AMIR (configurável via `-Zcomptime-fuel=N`).
  - **No LSP (Modo interativo)**: Padrão reduzido para `100.000` passos.
- A cada salto básico (`Branch`, `Goto`) e chamada de função, a VM desconta o contador. Se o fuel zerar, a execução aborta cooperativamente e emite um diagnóstico rico no editor:
  ```text
  error[T045]: limite de passos de execução em tempo de compilação excedido (fuel exhausted)
    --> src/main.aru:14:5
     |
  14 |     while (i < 10) { // loop não convergiu após 100.000 passos
     |     ^^^^^^^^^^^^^^
     = note: possível laço infinito em código de compilação
     = help: use -Zcomptime-fuel=<N> para aumentar o limite se esta computação for legítima
  ```

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

### Invariantes Preservados
1. **Pureza das Queries**: A AMIR VM proíbe sumariamente operações de I/O cru, chamadas de rede e leitura arbitrária de filesystem. O acesso a assets estáticos é intermediado estritamente por inputs tipados do Salsa.
2. **Determinismo Byte-a-Byte**: Avaliar a mesma expressão comptime em diferentes sistemas operacionais hospedeiros (Windows, Linux, macOS) produz exatamente a mesma representação de bytes para `ConstValue`.
3. **Ausência de Estado Global Mutável**: A VM não possui variáveis globais ou ponteiros compartilhados entre threads de análise.

### Desvantagens e Custos
- **Pressão sobre o Type Checker**: Executar código no meio da checagem de tipos introduz uma dependência de intercalação entre typeck, lowering de AMIR e avaliação. Isso é gerenciado através do isolamento de queries Salsa em grão fino.
- **Complexidade do Compilador**: A inclusão de um interpretador de bytecode SSA tipado (estilo Miri) exige testes rigorosos de conformidade semântica para garantir que a VM nunca discorde do código gerado pelo backend Cranelift ou C.

---

## 6. Racional e Alternativas (Rationale & Alternatives)

| Opção | Vantagens | Desvantagens | Veredito |
| :--- | :--- | :--- | :--- |
| **Rust Proc-Macros (`syn`/`quote`)** | Familiar para desenvolvedores Rust. | Tempo de compilação terrível; exige compilar DLLs no host; cegueira completa de tipos. | **Rejeitado** |
| **Rust `macro_rules!`** | Leve e sem crates dinâmicas. | Sintaxe paralela bizarra; difícil de manter; propensa a erros complexos de recursão. | **Rejeitado** |
| **Zig Comptime Puro** | Uma única linguagem; sem macros. | Executa cedo demais na AST; difícil de orquestrar com Salsa incremental e early-cutoff sem bugs de invalidação. | **Aprimorado (Zig Comptime + AMIR SSA)** |
| **C++ Preprocessor (`#define`)** | Simples de implementar. | Zero segurança; falta de higiene; bugs grotescos de substituição textual. | **Rejeitado** |

---

## 7. Arte Prévia (Prior Art)

- **Zig**: Pioneiro no paradigma de "comptime como única linguagem de metaprogramação", provando que 90% das macros podem ser substituídas por reflexão de tipos e laços desdobrados.
- **Miri (Rust)**: O padrão de ouro em interpretação de representação intermediária (MIR) com detecção de comportamentos indefinidos e layout de memória virtual estrito.
- **Circle C++ (Sean Baxter)**: Demonstrou o poder de extensão de sintaxe com `@meta` tipado e injeção de código diretamente em compiladores industriais de sistemas.
- **D Language**: Demonstrou a viabilidade prática de CTFE industrial e introspecção estática com `__traits` desde a década de 2000.
- **Mojo**: Demonstrou o uso de metaprogramação integrada com representações MLIR/SSA para otimização extrema de código científico.

---

## 8. Questões em Aberto (Unresolved Questions)

1. **Recursão de Derivações**: Qual o limite ideal de profundidade quando um atributo `@Derive` injeta uma interface cujo corpo invoca outra avaliação `comptime`?
2. **Gramática de Splicing em Nível de Itens**: A notação `${target.name}` é suficiente para declarações, ou precisaremos de marcadores adicionais para splicing de parâmetros e blocos inteiros?

---

## 9. Possibilidades Futuras (Future Possibilities)

- **DSLs Estáticas Compiladas para AMIR**: Validação e compilação em tempo de compilação de consultas SQL tipadas (`sql!("SELECT id, name FROM users")`), expressões regulares compiladas diretamente para autômatos finitos determinísticos (DFA) na memória constante, e formatadores de string verificados com zero overhead.
- **Compilação JIT de Comptime Pesado**: Para funções de tempo de compilação extremamente longas (ex: geração de tabelas trigonométricas gigantes ou pré-processamento de datasets), a AMIR VM poderá utilizar o próprio backend Cranelift para compilar a função em código de máquina nativo e executá-la com aceleração de hardware.
