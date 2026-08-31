# Arandu — Arquitetura da Standard Library (Stdlib)

> Documento de especificação de design e layout de pacotes do ecossistema de bibliotecas oficiais da linguagem Arandu.
>
> Objetivo: Garantir modularidade extrema, tempo de compilação previsível, suporte a ambientes bare-metal/embedded, compatibilidade nativa com *no_std* e preparidade para a auto-hospedagem (self-hosting) na versão 1.0.

---

## Visão Geral e Contexto

Este é o desenho arquitetural futuro das camadas oficiais `core`, `alloc`,
`std` e extensões, com foco em previsibilidade, embedded e self-hosting.

## Detalhes Técnicos da Implementação

### 🧠 Filosofia Central do Projeto

O Arandu prioriza acima de tudo:
1. **Previsibilidade semântica**: Sem mágica implícita de compilador ou runtime;
2. **Locality de memória**: Acesso linear a dados e otimização agressiva de cache;
3. **Throughput de compilação**: Tempos de ciclo edit-compile-run ultra-rápidos (<100ms);
4. **Baixo overhead**: Binários finais extremamente compactos e com zero overhead desnecessário;
5. **Tooling de alta qualidade**: Diagnósticos bonitos, ricos e integrados;
6. **Incrementalidade**: Grafo de queries fins do compilador para evitar retrabalho;
7. **Controle explícito**: O desenvolvedor tem controle absoluto sobre alocação de memória e runtime.

O projeto **NÃO** busca:
* Mágicas ou açúcares sintáticos complexos e implícitos que escondem custo de execução;
* Abstrações invisíveis custosas na heap ou que prejudiquem análise estática;
* Runtimes monolíticos ou acoplados de forma irreversível à linguagem;
* Otimizações obscuras em tempo de compilação impossíveis de auditar ou diagnosticar.

Isso define a identidade e direção de engenharia inteira da linguagem.

---

### 💎 Core Invariants (Bússola do Ecossistema)

O design de todas as APIs oficiais da linguagem Arandu (desde o compilador até a stdlib) deve seguir estritamente estes 8 invariantes fundamentais para evitar degradação de performance e arquitetura:

1. **No hidden allocation**: Alocações dinâmicas na heap nunca acontecem de forma implícita ou escondida. Se uma função aloca, ela deve receber um alocador explícito ou ser semanticamente óbvia (ex: retornar um tipo contido em `arandu_alloc`).
2. **No implicit runtime**: O usuário não paga por um runtime assíncrono ou modelo de concorrência global se não o utilizar.
3. **ID-based compiler architecture**: Grafos, nós sintáticos e entidades de IR evitam ponteiros puros de 64 bits em prol de compactos IDs de 32 bits indexando arenas densas.
4. **Stack-first execution**: Objetos pequenos e corrotinas utilizam alocação em stack por padrão. Alocações em heap são reservadas para escape real.
5. **Zero-cost abstractions**: Abstrações de alto nível (iteradores, enums, colorless async) devem compilar no mesmo código assembly gerado por implementações equivalentes de baixo nível escritas manualmente.
6. **Async is semantic, runtime is optional**: O suporte a `await` e corrotinas é resolvido semanticamente pelo compilador. Executores e reatores de I/O são modulares e substituíveis.
7. **no_std compatibility first**: Toda estrutura fundamental (no `arandu_core`) é nativamente independente de sistema operacional e heap.
8. **Cache locality over abstraction purity**: Prioriza-se layouts contíguos de memória (SoA, small vectors, bitsets) mesmo que isso exija quebrar paradigmas clássicos orientados a objetos.

---

### 📐 Filosofia de Camadas (The Layered Stdlib)

Para evitar os problemas comuns de linguagens de sistemas clássicas (como o acoplamento excessivo à alocação dinâmica no topo da stdlib ou a inclusão de frameworks mutáveis e pesados que envelhecem mal), a biblioteca oficial do Arandu é dividida em **quatro camadas estritas e isoladas**.

```text
 ┌────────────────────────────────────────────────────────┐
 │                      arandu_ext                        │  ← Gráficos, Física, Renderer, ECS
 └──────────────────────────┬─────────────────────────────┘
                            ▼
 ┌────────────────────────────────────────────────────────┐
 │                      arandu_std                        │  ← OS, Filesystem, Net, Async Runtime
 └──────────────────────────┬─────────────────────────────┘
                            ▼
 ┌────────────────────────────────────────────────────────┐
 │                      arandu_alloc                      │  ← Arenas, Box, Vec, Strings, HashMaps
 └──────────────────────────┬─────────────────────────────┘
                            ▼
 ┌────────────────────────────────────────────────────────┐
 │                      arandu_core                       │  ← Zero heap, Zero OS, Zero Threads
 └────────────────────────────────────────────────────────┘
```

---

### 1. `arandu_core` (A Camada Fundamental)

Esta camada é **estritamente livre de dependências externas**. Ela não assume a existência de um sistema operacional, de memória dinâmica (heap) global, nem de suporte a threads de kernel. É o bloco básico de construção para qualquer ambiente, incluindo bootloaders, kernels e firmware de microcontroladores de baixíssimo consumo.

### Estrutura de Módulos
```text
arandu_core
 ├─ mem          # Operações unsafe de memória, alinhamento e layout de tipos
 ├─ pointer      # Identidade segura de ponteiros (`null`/`isNull`)
 ├─ option       # Opcionalidade canônica (Option<T>)
 ├─ result       # Tratamento de erro monádico (Result<T, E>)
 ├─ iter         # Iteradores puros, adaptadores lazy combinatoriais
 ├─ slice        # Views contíguas de memória indexáveis ([T])
 ├─ math         # Operações matemáticas básicas, trig, float/integer bounds
 ├─ cmp          # Ord, Eq, PartialEq, comparação de dados
 ├─ hash         # Interfaces de hashing e algoritmos puros de hashing de bloco
 ├─ future       # Trait Future básico (implementado automaticamente pelo compilador para Coroutine[T])
 ├─ task         # Context, Waker, RawWaker (abstrações de execução)
 ├─ pin          # Abstrações de pinning de memória (Pin/Unpin gerais; corrotinas usam OSSA Indices livres de Pin)
 ├─ cell         # Interior mutabilidade controlada (Cell, RefCell, UnsafeCell)
 ├─ marker       # Marcadores fundamentais do compilador (Send, Sync, Copy, Sized, PhantomData)
 ├─ borrow       # Abstrações de empréstimo (Borrow, BorrowMut)
 ├─ fmt          # Formatação e diagnostics de baixo nível (Debug, Display, formatting engines)
 ├─ panic        # Handlers básicos de pânico e asserções estáticas
 ├─ intrinsics   # abort, abort_generational_mismatch (traps; zero heap)
 ├─ simd         # Tipos vetoriais e primitivas portáveis de SIMD (Fase A7)
 ├─ atomic       # Tipos atômicos puros suportados pelo hardware
 └─ arch         # Especificações arquiteturais específicas (x86_64, AArch64, RISC-V)
```

> [!IMPORTANT]
> **A Semântica de Async no Core**: Os blocos de controle de tarefas assíncronas (`Future`, `Waker` e `Poll`) e o tipo embutido `Coroutine[T]` pertencem ao ecossistema do `arandu_core`. O compilador gera corrotinas (`Coroutine[T]`) debaixo do capô e implementa automaticamente o interface `Future[T]` para elas. O runtime de execução assíncrona (threads, syscalls) vive em camadas superiores, mas a semântica abstrata e as máquinas de estado geradas pelo compilador para o `await` dependem do `arandu_core`.

---

### 2. `arandu_alloc` (Gerenciamento de Memória & Layouts)

Esta camada introduz o conceito de alocação de memória dinâmica. Ela depende do `arandu_core`, mas **não exige suporte a sistema operacional** (pode rodar no bare-metal se um alocador físico for fornecido à API).

### Estrutura de Módulos
```text
arandu_alloc
 ├─ allocator_api      # Traits para custom allocators (Arena, Slab, Bump, Global)
 ├─ arena              # Alocadores do tipo Bump, Growable Arena, Scratch Arenas
 ├─ slab               # Alocadores de tamanho fixo com reciclagem rápida por Free Lists
 ├─ gen_arena          # GenSlot / GenRef / GenArena — fallback geracional F2.3 (não em core)
 ├─ rc                 # Referência compartilhada thread-local (Reference Counting)
 ├─ arc                # Referência compartilhada thread-safe (Atomic Reference Counting)
 ├─ boxed              # Ponteiro exclusivo alocado na heap (`Box<T>`)
 ├─ vec                # Vetor dinâmico contíguo reajustável (`Vec<T>`)
 ├─ smallvec           # Vetores otimizados com inline-stack storage temporário (elimina heap allocs pequenos)
 ├─ string             # String dinâmica UTF-8 editável (`String`)
 ├─ hashmap            # Tabela Hash de alta performance (Robin Hood / Swiss Table)
 ├─ bitset             # Primitivas e estruturas genéricas de conjuntos de bits (dense arrays, roaring bitmaps)
 └─ arena_collections  # Coleções otimizadas para viver estritamente dentro de Arenas
```

> **F2.3 / GenRef:** `gen_arena` is the only place dynamic generational tables live.
> Trap on mismatch is `std.core.intrinsics.abort_generational_mismatch`.
> Compiler ABI: `docs/arandu-genref-gold-rfc-v0.1.md`.

### Filosofia de Alocadores Customizados
Diferente de linguagens tradicionais onde todas as coleções apontam implicitamente para um único alocador global, todas as estruturas em `arandu_alloc` suportam um parâmetro genérico opcional de alocador:
```arandu
// Por padrão usa o GlobalAllocator da plataforma:
list: Vec<i32> = Vec::new()

// Alocação focada em performance local (Zero Heap Global):
local_arena = BumpArena::new(64 * 1024) // 64KB
list: Vec<i32, &BumpArena> = Vec::new_in(&local_arena)
```

---

### 3. `arandu_std` (O Sistema Operacional & Rede)

Esta camada expõe serviços clássicos fornecidos por sistemas operacionais modernos. Ela unifica as APIs assíncronas com as primitivas do OS.

### Estrutura de Módulos
```text
arandu_std
 ├─ io           # Leitura e escrita síncrona/assíncrona (Read, Write, BufRead)
 ├─ fs           # Acesso ao sistema de arquivos (File, Metadata, Permissions)
 ├─ process      # Spawning de processos, redirecionamento de I/O, IPC
 ├─ env          # Variáveis de ambiente, caminhos de executáveis, argumentos CLI
 ├─ path         # Manipulação e validação de caminhos de arquivos (Path, PathBuf)
 ├─ time         # Medição de tempo monotônico e absoluto, instantes, durações
 ├─ random       # Geração de números pseudo-aleatórios e criptograficamente seguros
 ├─ thread       # Threads de sistema nativas, local storage, handles de join
 ├─ ffi          # Conversão de tipos, ponteiros C, carregamento dinâmico (dlopen)
 ├─ collections  # Coleções avançadas (BTreeMap, Deque, LinkedList) dependentes de alocação e OS
 ├─ json         # Parser e serializador JSON ultra rápido, stream-based, cache-friendly
 ├─ xml          # Parser XML minimalista focado em conformidade e velocidade
 ├─ crypto       # Criptografia modular (hashes, cifras, TLS/X.509)
 ├─ testing      # Testes e benchmarks nativos; ver arandu-testing-benchmark-harness-v0.1.md
 ├─ os           # Módulos específicos de sistema operacional (Linux, Windows, macOS)
 │   ├─ descriptors # Handles puros, file descriptors, pipe descriptors
 │   ├─ memory      # Controle avançado de memória virtual (mmap, VirtualAlloc, commit/reserve)
 │   └─ syscalls    # Interface direta a raw syscalls da plataforma
 ├─ net          # Sub-sistema de rede unificado
 │   ├─ tcp      # Sockets TCP (Listen, Stream)
 │   ├─ udp      # Sockets UDP (Datagrams, Multicast)
 │   ├─ dns      # Resolução de nomes assíncrona
 │   ├─ http     # Cliente e servidor HTTP/1.1 e HTTP/2 embarcado de baixo overhead
 │   └─ websocket# Suporte nativo a WebSocket sobre a pilha HTTP/TCP
 ├─ sync         # Primitivas de sincronização avançadas
 │   ├─ mutex    # Mutex de exclusão mútua adaptativo (Spin + Park)
 │   ├─ rwlock   # Reader-Writer Lock justo
 │   ├─ condvar  # Variáveis de condição
 │   ├─ barrier  # Barreiras de sincronização de threads
 │   ├─ once     # Inicialização lazy e thread-safe única
 │   ├─ channel  # Canais de comunicação (MPMC, SPSC, flume-style)
 │   └─ parking  # Interface low-level de thread parking (para custom mutexes)
 └─ runtime      # O motor de concorrência assíncrona da linguagem
     ├─ scheduler# Agendamento cooperativo/work-stealing de tarefas com afinidade NUMA
     ├─ executor # Thread-pool pool global e spawning de tasks assíncronas
     ├─ reactor  # Despachante de eventos de I/O do SO
     ├─ epoll    # Reactor backend para Linux
     ├─ kqueue   # Reactor backend para macOS/BSD
     ├─ iocp     # Reactor backend para Windows
     └─ io_uring # Reactor de alta vazão moderno para Linux kernel ≥ 5.1
```

---

### 4. `arandu_ext` (Extensões Opcionais de Aplicação)

Para manter o núcleo da linguagem focado e evitar a obsolescência acelerada da API, frameworks de nicho que tradicionalmente são embutidos em stdlibs monolíticas são movidos para `arandu_ext`. Eles são distribuídos junto com a SDK do Arandu, mas importados separadamente e não poluem o binário final se não forem explicitamente referenciados.

### Estrutura de Módulos
```text
arandu_ext
 ├─ ecs           # Entity-Component-System nativo e de alta performance
 ├─ game          # Utilitários de loop de jogo, time steps fixos, input handling
 ├─ serialization # Parsers rápidos para JSON, XML e formatos estruturados de dados (arandu_data)
 ├─ renderer      # Abstração de hardware gráfico de baixo overhead (WGPU/Vulkan/D3D12 wrapper)
 ├─ audio         # Mixagem, decodificação de áudio (WAV, OGG, MP3) e output espacial
 ├─ media         # Carregamento e manipulação de texturas, imagens (PNG, JPG) e codecs básicos
 ├─ gui           # Framework declarativo e imediato de interface gráfica 2D
 └─ physics       # Integração com física 2D/3D (colisões, gravidade, corpos rígidos)
```

---

### 🎯 Modularidade Criptográfica (`crypto`)

Para mitigar o aumento do tempo de compilação e do tamanho do executável gerado, o módulo `arandu_std::crypto` é estruturado em sub-módulos independentes de forma estrita.

```text
crypto
 ├─ hash    # SHA-256, SHA-3, BLAKE3 (Compila rápido, usado em hashing de dados)
 ├─ aes     # Cifras simétricas por hardware
 ├─ rsa     # Assinaturas clássicas e chaves assimétricas
 ├─ ed25519 # Algoritmos modernos de curvas elípticas compactas
 ├─ tls     # Interface de sockets seguros com TLS 1.3
 └─ x509    # Validação de certificados digitais
```
* **Tree-shaking estrito**: Se o usuário importar apenas `crypto::hash::sha256`, o linker do compilador descarta fisicamente todo o suporte a RSA, curvas elípticas e TLS.
* **Sem dependência de OpenSSL**: O Arandu implementa seus primitivos criptográficos principais em código gerenciado ou expõe APIs nativas da plataforma (como Windows CNG ou macOS Security framework) para manter o tamanho do binário próximo a zero e evitar dores de cabeça com cross-compilation.

---

### 🚫 O Que NÃO Pertence à Stdlib

Para preservar a estabilidade da API, o tempo de compilação, o tamanho de binários finais e a modularidade global, o Arandu ativamente evita incluir na stdlib básica os seguintes componentes:
* **Game engines** e loops especializados de gameplay;
* **ECS frameworks** de aplicação específica;
* **GUI frameworks** declarativos ou imediatos;
* **ORMs** (Object-Relational Mapping);
* **Web frameworks** complexos de alto nível (ex: routers MVC, templates);
* **Serializadores pesados** de domínio específico;
* **Renderers** 2D ou 3D acoplados a hardware gráfico;
* **Matemática especializada** de domínio complexo (ex: CAD, ML/AI, criptografia customizada avançada).

Todos estes componentes residem estritamente nas extensões de ecossistema (`arandu_ext`) ou em pacotes de terceiros distribuídos por gerenciadores de dependência.

---

### 🛠️ O Caminho para a Versão 1.0 (Self-Hosting)

O marco definitivo de maturidade da linguagem Arandu é o **Self-Hosting**: a capacidade de compilar o próprio compilador `arandu` escrito em Arandu.

### Critérios de Sucesso para a 1.0

1. **Auto-Hospedagem Estável**:
   O compilador original (escrito em Rust) compila o compilador Arandu escrito em Arandu. O binário gerado (Arandu-Compiler-V1) deve então compilar o código fonte do compilador Arandu de forma 100% equivalente, gerando binários idênticos byte a byte (verificação de convergência em 3 passos).

2. **Bootstrapping com Zero Dependências de Rust**:
   Ao atingir a 1.0, o compilador Rust é descartado da árvore de compilação oficial. Novas releases do Arandu utilizam o binário estável anterior do Arandu para compilar as próximas modificações.

3. **Stdlib Completa até `arandu_std::runtime`**:
   O compilador do Arandu utiliza compilação paralela maciça baseada no `arandu_std::runtime` (scheduler work-stealing A8) e consultas memoizadas (`arandu_core::hash` e `arandu_std::collections`).

4. **Desempenho Paritário ou Superior**:
   O tempo de compilação do compilador Arandu compilado por ele mesmo deve ser inferior a 3 segundos para cold-builds inteiros do próprio compilador, validando a arquitetura linear e orientada a dados (A5–A11).

## Relatório final AUD.5 — segurança de borrowed views

> Este relatório registra o corte da auditoria anterior à campanha. BV.1 já
> removeu as limitações do resumo escalar e de múltiplas origens descritas
> abaixo; propagação completa de holders em carriers continua pertencendo a
> BV.2, portanto `SL_S-Core` permanece parcial.

**Decisão:** suporte global **parcial**. O Arandu possui referências seguras
reais e um contrato interprocedural restrito; porém ainda não possui borrowed
views seguras para slices, strings ou carriers estruturais. Essa classificação
fecha a auditoria sem promover `SL_S-Core` a Gold.

### Matriz de capacidade

| Superfície | Classificação | Evidência e limite |
| --- | --- | --- |
| Borrow local `ref T`/`mut ref T` | **Suportado** | loans têm owner, kind, holders e live range; conflitos de move, mutação e destroy emitem O002/O003/O006 |
| Retorno direto ou união de parâmetros emprestados | **Suportado em BV.1** | interface derivada do fluxo contém somente origens alcançáveis; retorno de local é O010 |
| Forwarding, import e especialização genérica | **Suportado em BV.1** | testes CLI/query cobrem composição, recursão, import, genérico e hash canônico |
| Incrementalidade e IDE | **Suportado para o contrato BV.1** | mudança do summary invalida caller; mudança só de corpo para no `borrow_interfaces` HashEq |
| `Use`, `Load`, `Store` e block args intraprocedurais | **Parcial** | o fixpoint preserva holders conhecidos, mas o corpus público completo de branches, loops e projeções ainda não foi fechado |
| Receiver/método, projeções de campo e suspensão | **Parcial** | existem fundações locais e checks conservadores; não há prova black-box suficiente para uma API pública de view retornada |
| Caminhos de `Option`, `Result`, tuple, struct e enum | **Contrato BV.1; holders BV.2 pendentes** | forma estrutural e `Option<ref T>` já são demonstradas; construção/projeção/destructuring completos ainda não são Gold |
| `Slice<T>` e views de `String` | **Ausente como API segura** | `Slice` continua `ptr[T] + len`; `get` retorna `Option<ptr[T]>`, sem manter owner vivo |
| Retorno com múltiplas origens possíveis | **Suportado em BV.1** | branches fazem união apenas das fontes que chegam ao retorno; candidato só compatível pelo tipo é excluído |
| Closure escapável, global/heap e ida-e-volta por ponteiro cru | **Ausente** | não existe vínculo estático recuperável; ponteiro cru continua obrigação `unsafe` |

“Ausente” não significa que o compilador aceita silenciosamente o caso. Nos
casos que alcançam a superfície segura, a política é **fail-closed**: rejeitar
é preferível a fabricar uma origem, promover para GenRef ou transformar um
ponteiro tipado em referência aparentemente segura.

### Resultado dos gates

| Gate da auditoria | Resultado |
| --- | --- |
| Origem direta preservada por call/forwarding/genérico/import | **Passou** |
| O002/O003/O006/O010 nascem e desaparecem após cache hit | **Passou** |
| Summary participa de hash, invalidação e early-cutoff Salsa | **Passou** |
| Diagnóstico CLI/LSP equivalente, estruturado e sem publicação stale | **Passou** |
| Relatório AMIR determinístico localiza fronteiras de perda | **Passou** |
| Corpus integral de aggregates, closures, async, projeções e múltiplas origens | **Não passou — não implementado** |
| Views seguras `Slice.get`, `String.asStr` e `String.asBytes` | **Não passou — continuam deliberadamente indisponíveis** |
| Paridade C/Cranelift de todo o corpus de borrowed views | **Não passou — só o subconjunto direto possui cobertura** |

Os três últimos resultados impedem Gold, mas não impedem encerrar a auditoria:
eles fornecem precisamente a decisão requerida — a fundação direta é segura,
enquanto a abstração de borrowed view ainda precisa de um contrato estrutural.

### Decisão arquitetural permanente

1. A sintaxe canônica permanece `T`/`own T`, `ref T` e `mut ref T`; não será
   adicionada sintaxe de lifetime enquanto um caso real não exigir isso.
2. `ReturnBorrowSummary` estrutural e de múltiplas fontes é a fronteira pública
   desde BV.1; não contém spans, `TypeId` ou ordem incidental de mapa.
3. `Option`/`Result` e demais carriers exigirão uma dependência não escapável
   estrutural, preservada em construção, match, cópia, phi e chamada.
4. `Slice` e `String` não podem anunciar views seguras enquanto continuarem
   baseadas apenas em `ptr + len`.
5. GenRef, RC/ARC e heap promotion não substituem uma prova de borrowed view.
6. APIs de ponteiro só contam como alternativa quando forem publicamente
   `unsafe` e documentarem a obrigação do chamador.

### Dívida técnica e sequência futura

- fazer aggregates participarem do fixpoint de holders e rejeitar escapes em
  construção, destructuring e pattern matching;
- fechar corpus de receiver/método, branch/loop, reborrow, closure e suspensão;
- provar invalidação por `Vec.push/reserve` e `String.pushStr/reserve` enquanto
  uma view estiver viva;
- somente então introduzir `Slice.get -> Option<ref T>`, `String.asStr` e
  `String.asBytes`, com paridade C/Cranelift e regressões CLI/LSP.

## PONTOS DE MELHORIA (O que não está no roadmap)

Grande parte deste documento ainda é design, não promessa implementada. Metas
numéricas de self-hosting precisam de corpus, hardware e protocolo de benchmark
definidos antes de se tornarem gate.

O primeiro corte de `SL_S-Core` estabelece os seguintes contratos concretos:

- nomes públicos de funções e métodos usam `camelCase`;
- `char` representa um Unicode scalar em 32 bits em layout, C e Cranelift;
- `Option`/`Result` consomem `self` ao retirar payloads possuídos;
- `Slice` é, neste corte, uma **raw non-owning view**: seu ABI é `ptr[T] + len`
  e ela não mantém o owner vivo nem carrega uma proveniência de borrow entre
  chamadas. `Slice.get` retorna `Option<ptr[T]>`; a leitura do ponteiro continua
  explícita e `unsafe` em `std.core.mem`. Isso não é uma borrowed view segura;
  `Vec.asSlice`/`String.asBytes` só poderão ser APIs seguras quando a dependência
  estrutural de ownership estiver implementada e validada;
- `ref T`/`mut ref T` são os tipos de referência segura da linguagem; `*expr` é a
  operação de dereference correspondente. Para `ptr[T]`, `*expr` permanece
  permitido somente em `unsafe`;
- a auditoria AUD.0–AUD.5 possui evidência AMIR determinística para loans,
  holders, escapes e perdas de origem. Retornos diretos são suportados por
  `ReturnBorrowSummary`; BV.1 adicionou múltiplas origens e
  caminhos estruturais, mas a classificação global permanece parcial até que
  holders em todos os carriers atravessem construção e destructuring em BV.2;
- `std.core.pointer` expõe somente identidade segura. Aritmética, leitura e
  escrita de ponteiros permanecem intrínsecos `unsafe`;
- aritmética checked de `int` testa limites antes da operação e deriva os
  limites da largura do target.

Ainda impedem a promoção para Gold: allocator genérico real, API fallible para
OOM, views emprestadas de slices/strings, `String.asStr`/`asBytes`/`pushStr`,
drop de payloads não triviais e cobertura nativa publicada em 32 e 64 bits.
O alias histórico `std.core.ptr` foi removido porque oferecia aritmética de
ponteiro através de uma função aparentemente segura; uma compatibilidade futura
só pode voltar junto de funções públicas `unsafe` no contrato da linguagem.

## Futuro e Próximos Passos

Implementar a stdlib na ordem do roadmap mestre, publicando apenas módulos com
contrato, tests/benchmarks e suporte coerente aos targets declarados.
