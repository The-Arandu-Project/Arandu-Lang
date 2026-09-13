# RFC 0014: Backend WebAssembly Nativo com Component Model (WIT), Compilação Incremental e Paralelismo Determinístico

- **Número da RFC:** 0014
- **Título:** Backend WebAssembly Nativo com Component Model (WIT), Compilação Incremental e Paralelismo Determinístico
- **Autor(es):** Equipe Arandu
- **Data de Início:** 2026-09-13
- **Status:** `Draft`
- **Área Principal:** `Backend`
- **PR da RFC:** N/A
- **Issue de Acompanhamento:** N/A

---

## 1. Resumo (Summary)

Esta RFC propõe a criação do **`arandu_backend_wasm`**, um backend WebAssembly (`wasm32`) de primeira classe e nativo para o compilador Arandu. Diferente das abordagens convencionais do mercado — que sofrem com compilações lentas via LLVM, binários inflados e milhares de linhas de cola em JavaScript geradas por ferramentas como `wasm-bindgen` ou `Emscripten` —, a proposta projeta uma arquitetura além do estado da arte:

1. **Emissão Direta do WebAssembly Component Model (WASI Preview 2 / WIT)** a partir do AMIR, sem intermediação de C ou LLVM, e com **zero cola em JavaScript**.
2. **Compilação Incremental Sub-15ms** através da integração estreita com o motor de queries puras **Salsa**, permitindo recarregamento a quente (*hot-reloading*) instantâneo no navegador.
3. **Mapeamento Direto de Interfaces Arandu para WIT e Canonical ABI**, permitindo a troca de dados estruturados (strings, records, variants, slices) sem serialização ou cópias intermediárias.
4. **Gerenciamento de Memória Linear Baseado em Arenas Geracionais (`GenRef`)**, eliminando a necessidade de garbage collectors pesados ou fragmentação por `malloc` tradicional.
5. **Paralelismo Estruturado Multi-Thread no Navegador** baseado em Web Workers e `SharedArrayBuffer` com primitivas atômicas, preservando a garantia formal de determinismo e ausência de *data races* da [RFC 0003](0003-structured-parallelism.md).
6. **Estratégia de Codegen Dual:** Geração pura em Rust via `wasm-encoder` em modo desenvolvimento e pós-processamento opcional com `wasm-opt` (Binaryen) em modo release.

---

## 2. Motivação (Motivation)

O WebAssembly tornou-se a camada de execução universal não apenas para a Web, mas também para ambientes serverless, edge computing (Cloudflare Workers, Fastly Compute, AWS Lambda) e sistemas de plugins isolados. No entanto, as linguagens existentes enfrentam compromissos severos:

* **Rust / `wasm-bindgen`**: Produz binários rápidos, mas o ciclo de desenvolvimento é arrastado (LLVM consome segundos/minutos). A interoperabilidade com JavaScript depende de um runtime complexo de shims em JS (`wasm-bindgen`), tabelas de ponteiros opacos e alocações frequentes na memória linear para decodificação UTF-8.
* **C/C++ / `Emscripten`**: Emula um sistema operacional POSIX completo dentro do navegador, gerando centenas de kilobytes de código de infraestrutura para simular sistemas de arquivos, sinais e syscalls desnecessárias.
* **Go / Kotlin / Dart**: Ou embutem um runtime de Garbage Collector pesado (inflando o binário para megabytes), ou dependem exclusivamente do WasmGC, o que fragmenta a compatibilidade com ambientes de servidor (Wasmtime, Wasmer) e impede controle preciso de layout de memória.
* **`asyncify`**: Para dar suporte a I/O assíncrono sem reescrever o código, a maioria das toolchains emprega o passo `asyncify` do Binaryen, que duplica o tamanho do bytecode e degrada o desempenho ao desbobinar e rebobinar pilhas de execução em memória linear.

O compilador Arandu possui características arquiteturais únicas que permitem contornar todos esses gargalos:
* Já possui um **`DataLayout` parametrizável** (`DataLayout::ptr_width(4)`) testado e validado para 32 bits (ILP32).
* A **`std.core` é desacoplada do host** (zero-heap, sem syscalls obrigatórias de SO).
* O **modelo de memória semântica e `GenRef`** viabiliza segurança espacial de memória sem necessidade de tracing GC.
* O sistema de **queries incrementais Salsa** permite reemitir apenas as funções e componentes alterados.

Se esta RFC não for adotada, o suporte a Wasm dependerá eternamente de compilar o backend C gerado via `clang`/`emscripten`, perdendo os benefícios de compilação incremental sub-segundo, tipagem estruturada de componentes e eficiência em tamanho de binário.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

### 3.1. Compilando para WebAssembly

Compilar um projeto Arandu para WebAssembly Component Model requer apenas especificar o target no CLI:

```bash
# Compilação rápida incremental para desenvolvimento no navegador
arandu build --target wasm32-wasi

# Compilação otimizada para produção (com dead-code elimination e minificação)
arandu build --target wasm32-wasi --release
```

O compilador emite um arquivo `.wasm` padronizado. O manifesto do projeto pode declarar interfaces exportadas diretamente no `Arandu.toml`:

```toml
[package]
name = "image_processor"
version = "0.1.0"
target-type = "component"

[wasm]
memory-initial-pages = 2
enable-threads = true
```

### 3.2. Exportando e Importando Interfaces (WIT First)

Em Arandu, as interfaces declaradas no código são compiladas diretamente como interfaces do WebAssembly Component Model:

```arandu
module app.filter

import std.core.slice as slice

public struct Pixel {
    pub r: u8
    pub g: u8
    pub b: u8
    pub a: u8
}

public interface GrayscaleFilter {
    func apply(pixels: mut ref []Pixel): uint
}

public struct LuminanceFilter {
    factor: float
}

public func LuminanceFilter.apply(self: mut ref LuminanceFilter, pixels: mut ref []Pixel): uint {
    let len = slice.len<Pixel>(*pixels)
    let mut i = 0
    while i < len {
        let p = pixels[i]
        let gray = ((p.r as float * 0.299) + (p.g as float * 0.587) + (p.b as float * 0.114)) as u8
        pixels[i] = Pixel { r: gray, g: gray, b: gray, a: p.a }
        i = i + 1
    }
    return len
}
```

O compilador gera automaticamente a definição de interface WIT e o componente Wasm canônico:

```wit
package app:filter;

interface grayscale {
    record pixel {
        r: u8,
        g: u8,
        b: u8,
        a: u8,
    }
    apply: func(pixels: list<pixel>) -> u32;
}

world filter-service {
    export grayscale;
}
```

No JavaScript moderno ou no Wasmtime, esse componente é consumido diretamente sem nenhuma biblioteca intermediária:

```javascript
import { grayscale } from './image_processor.wasm';

// Passa arrays diretamente via TypedArray / SharedArrayBuffer sem cola JS
const pixels = [ { r: 255, g: 0, b: 0, a: 255 } ];
const count = grayscale.apply(pixels);
```

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

### 4.1. Arquitetura do Crate `arandu_backend_wasm`

O novo crate será adicionado ao workspace mantendo a separação rigorosa de responsabilidades:

```
crates/
  arandu_backend_wasm/
    Cargo.toml
    src/
      lib.rs                 # Ponto de entrada e API de emissão
      component/             # Geração da Canonical ABI e seções de componentes
        canonical.rs         # Lowering e lifting de tipos primitivos, records e listas
        wit_export.rs        # Geração de metadados e exports de interface
      translator/            # Tradutor de AMIR para WebAssembly
        func.rs              # Tradução de funções e blocos
        expr.rs              # Operandos, rvalues e operações aritméticas
        stackify.rs          # Algoritmo de stackification exata de CFG
      memory/                # Alocador linear e mapeamento de páginas
        bump_allocator.rs    # Micro-alocador linear para standalone Wasm
        arena_runtime.rs     # Suporte Wasm para GenRef e fallbacks
      emit.rs                # Codificação em bytecode binário via `wasm-encoder`
```

### 4.2. Algoritmo de Stackification Exato de CFG (Sem `asyncify`)

Diferente das CPUs tradicionais que aceitam saltos arbitrários (`jmp`), o WebAssembly possui um modelo de pilha estruturado com blocos delimitados (`block`, `loop`, `if`, `br`, `br_if`, `br_table`).

Para traduzir o CFG de blocos básicos da AMIR para blocos estruturados sem a penalidade de tamanho e desempenho do `asyncify`, o `arandu_backend_wasm` implementará o algoritmo **Fast and Exact Stackifier**:
1. Identificação de laços e árvores de dominância no grafo de controle de fluxo de AMIR.
2. Criação de escopos `loop` para predecessors em retorno (*back-edges*) e `block` para saltos à frente (*forward-edges*).
3. Uso de `br_table` para saltos multifurcados e máquinas de estado de corrotinas assíncronas, garantindo execução em loop plano com despacho por rótulo em O(1), sem salvar pilhas de frames inteiras na memória linear.

### 4.3. Canonical ABI do Component Model

Para tipos complexos trocados entre o host e o módulo Wasm, o backend implementará diretamente a Canonical ABI da Bytecode Alliance:
* **Escalares e Booleanos**: Mapeados para `i32`, `i64`, `f32`, `f64`.
* **Slices (`[]T`) e Strings (`str`)**:
  * Em 32-bit, representados como um par `(ptr: i32, len: i32)` de 8 bytes no stack do Wasm.
  * O lifting e lowering transfere o controle de memória diretamente sem duplicação de buffers.
* **Structs e Registros**: Empacotamento alinhado estritamente pelas regras de `DataLayout::ptr_width(4)`.

### 4.4. Memória Linear e Arenas Geracionais (`GenRef`)

No ambiente Wasm:
* O módulo define uma única memória linear inicial: `(memory (export "memory") 2)` (128 KB).
* **Bump Allocator Embutido**: O runtime emite um alocador estático minimalista (< 150 bytes em Wasm) que gerencia o ponteiro da heap `__heap_base` e expande via `memory.grow` quando necessário.
* **Arenas Geracionais (`GenRef`)**:
  * Em vez de alocar blocos individuais na heap, o `GenArena` reserva páginas inteiras da memória Wasm.
  * O acesso por geração e índice se beneficia do fato de que qualquer leitura fora dos limites da memória linear já dispara uma exceção em hardware (`out of bounds memory access`), eliminando verificações redundantes no código quente.

### 4.5. Paralelismo Estruturado no Navegador

Para viabilizar o `parallel.aru` e o `WorkerPool` em WebAssembly:
* O módulo declara a flag de memória compartilhada: `(memory (export "memory") 4 64 shared)`.
* As operações de sincronização do scheduler de workers usam as instruções da proposta Wasm Threads:
  * `i32.atomic.wait` / `i64.atomic.wait` para suspensão não-ocupante de workers ociosos.
  * `atomic.notify` para acordar tarefas filhas concluídas.
* No navegador, o runtime inicializa uma pool de `Worker` JavaScript que compartilham a mesma instância do `WebAssembly.Memory`.
* Como o sistema de tipos de Arandu garante que referências e variáveis não-Copy não compartilham estado mutável sem proteção, o código multithread no navegador opera com **zero risco de condições de corrida**.

### 4.6. Ciclo de Compilação e Otimização Dual

```
                     [Código Fonte .aru]
                              │
                              ▼
                   [Análise Salsa / AMIR]
                              │
               ┌──────────────┴──────────────┐
               ▼                             ▼
       (Modo Dev / Fast)            (Modo Release / AOT)
               │                             │
    [arandu_backend_wasm]         [arandu_backend_wasm]
   (Emissão em <15ms via)        (Geração de Componente)
       `wasm-encoder`                        │
               │                             ▼
               │                     [`wasm-opt` (Binaryen)]
               │                  (DCE, Loop-Opt, Inlining)
               │                             │
               ▼                             ▼
   [Módulo .wasm funcional]       [Componente Wasm < 20KB]
  (Carregado pelo DevServer)     (Pronto para Produção / Edge)
```

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

### 5.1. Preservação dos Invariantes de Arquitetura do Arandu

1. **Salsa Ownership**: O `arandu_backend_wasm` será um crate consumidor puro. Nenhuma execução de queries Salsa residirá nele; a emissão será acionada a partir de queries orquestradas pelo `arandu_query` e `arandu_cli`.
2. **Determinismo e Ausência de Efeitos**: A geração do bytecode Wasm é uma função pura de `(AmirProgram, TypeCheckResult, DataLayout) -> Vec<u8>`. Dois builds com o mesmo código geram hashes SHA-256 e BLAKE3 idênticos byte a byte.
3. **DataLayout Canônico**: Todos os cálculos de deslocamento, empacotamento de structs e tamanho de descritores obedecem estritamente a `DataLayout::ptr_width(4)`. Nenhum offset mágico `+8` ou presunção de 64-bit é introduzido.
4. **Resiliência a Erros**: O tradutor de AMIR não utiliza `panic!`, `unwrap` ou `expect`. Violações de invariantes de tipos ou de controle de fluxo produzem `Diagnostic::ice`.

### 5.2. Desvantagens e Complexidades

* **Complexidade do Algoritmo de Stackify**: O mapeamento de um grafo de controle de fluxo arbitrário para blocos aninhados do Wasm requer testes rigorosos com laços aninhados, `break`, `continue` e saltos condicionais complexos.
* **Dependência Opcional do `wasm-opt`**: Para atingir tamanhos de binário microscópicos em modo release, o compilador poderá depender de um binário do `wasm-opt` embutido ou opcional na máquina do usuário.

---

## 6. Racional e Alternativas (Rationale & Alternatives)

### 6.1. Por que não usar o LLVM para Wasm?
* **Rejeitado**. O LLVM impõe penalidades severas em tempo de compilação (dezenas de segundos por build), necessita de gigabytes de código C++ compilado e não suporta o Component Model / WIT nativamente sem tooling externo complexo. Adicionar LLVM violaria o objetivo do Arandu de manter compilações interativas em milissegundos.

### 6.2. Por que não compilar através do backend C com Emscripten?
* **Rejeitado para produção**. Emscripten gera bibliotecas gigantescas de emulação POSIX em JS que não são necessárias para o Arandu. A abordagem via C permanece útil apenas como ferramenta de teste comparativo de paridade de backend.

### 6.3. Por que não compilar via Cranelift Wasm?
* O Cranelift é um backend que compila **de Wasm para código de máquina nativo** (x86_64, aarch64), e não de um IR de alto nível para bytecode `.wasm`. Ele não possui gerador de Wasm.

---

## 7. Arte Prévia (Prior Art)

* **WebAssembly Component Model Specification** (Bytecode Alliance / W3C): Padronização moderna de interfaces tipadas entre módulos isolados sem cola em JavaScript.
* **Fast and Exact Stackification of Control Flow** (V8 / Bytecode Alliance): Fundamentação teórica do algoritmo de reestruturação de CFG sem overhead de pilha.
* **`wasm-encoder` (Bytecode Alliance)**: Crate Rust de altíssimo desempenho para escrita de seções e instruções binárias Wasm sem alocações supérfluas.
* **Binaryen / `wasm-opt`**: O padrão da indústria para super-otimização e minificação de bytecode Wasm de release.
* **Rust `wasm-bindgen` & Zig Wasm Target**: Lições aprendidas sobre o custo da cola de código e os benefícios de interfaces minimalistas desacopladas de SO.

---

## 8. Questões em Aberto (Unresolved Questions)

1. **Estratégia de Empacotamento do `wasm-opt`**: Avaliar se o binário do `wasm-opt` deve ser embutido diretamente no CLI do Arandu via link estático de biblioteca C++ ou instalado sob demanda como componente de toolchain.
2. **APIs de Efeitos no Wasm (A2)**: Como o sistema de efeitos da linguagem mapeará capacidades do sistema no navegador (ex: requisições HTTP mapeadas para a API `fetch` nativa ou para `wasi:http`).
3. **Distribuição do Runtime de Workers**: Formalizar o template de inicialização do `WorkerPool` em ambientes web headless (Node.js/Deno/Bun vs Browser Window).

---

## 9. Possibilidades Futuras (Future Possibilities)

1. **Auto-Hospedagem no Navegador (In-Browser Arandu Playground)**: Com o backend Wasm e a compilação Salsa sub-15ms, o compilador Arandu inteiro poderá rodar no navegador do usuário, fornecendo um ambiente interativo (REPL/IDE) instantâneo sem necessidade de servidores de backend.
2. **Integração com WebGPU**: Mapeamento do paralelismo estruturado para pipelines de computação em GPU no navegador via shaders WGSL.
3. **Módulos Poliglotas Universais**: Publicação de pacotes Arandu que podem ser importados transparentemente em projetos Python, JavaScript, Go ou Rust como componentes isolados e seguros.
