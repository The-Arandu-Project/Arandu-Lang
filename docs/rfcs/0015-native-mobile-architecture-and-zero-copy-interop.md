# RFC 0015: Arquitetura Mobile Nativa, Interoperabilidade Zero-Copy e Pipeline de Bindings Multiplataforma

- **Número da RFC:** 0015
- **Título:** Arquitetura Mobile Nativa, Interoperabilidade Zero-Copy e Pipeline de Bindings Multiplataforma
- **Autor(es):** Equipe Arandu
- **Data de Início:** 2026-09-13
- **Status:** `Draft`
- **Área Principal:** `Backend` / `Tooling`
- **PR da RFC:** N/A
- **Issue de Acompanhamento:** N/A

---

## 1. Resumo (Summary)

Esta RFC define a arquitetura oficial do compilador Arandu para desenvolvimento de aplicações móveis de alto desempenho nos ecossistemas **Android (AArch64/x86_64)** e **iOS (AArch64/Simulador)**.

Rejeitando o compromisso histórico entre desenvolvimento unificado lento e desenvolvimento nativo duplicado, o Arandu estabelece o padrão **Headless Core com Zero-Copy Data Plane e UI 100% Nativa**:
1. **Compilação Nativa AOT AArch64** do núcleo de lógica de negócios, rede, persistência e algoritmos complexos, gerando artefatos estáticos e dinâmicos altamente otimizados (`.aar` para Android e `.xcframework` para Apple).
2. **Eliminação do Choque de Runtimes (ARC vs Tracing GC)**: Graças ao modelo de memória semântica com ownership linear e referências geracionais determinísticas (`GenRef` — [RFC 0001](0001-generational-fallback-genref.md) e [RFC 0007](0007-semantic-memory-model.md)), o Arandu opera sem tracing GC global, eliminando pausas de coleta e conflitos de retenção tanto no Android Runtime (ART) quanto no Swift ARC.
3. **Plano de Dados Zero-Copy via FFI Direta**: Slices e estruturas contíguas do Arandu mapeiam diretamente para `java.nio.DirectByteBuffer` (Android) e `UnsafeRawBufferPointer` / `ContiguousArray` (Swift) sem marshaling, serialização intermediária ou alocações redundantes.
4. **Chamadas JNI Ultra-Rápidas via CriticalNative/FastNative**: O gerador de stubs Android emite anotações `@CriticalNative` e `@FastNative` do ART, reduzindo o custo de transição entre a JVM e o código nativo ao patamar de uma chamada C padrão (ordem de nanossegundos).
5. **Síntese Incremental de Bindings Idiomáticos via Salsa**: Anotações semânticas (`@mobile`, `@mobile(async)`, `@mobile(event)`) disparam queries puras no compilador que sintetizam código Kotlin com Coroutines (`suspend fun`), `StateFlow` e Compose `@Composable`, bem como código Swift moderno com `async/await`, `AsyncSequence` e `@Observable` do SwiftUI.
6. **Isolamento de Concorrência e Garantia de 120 FPS (Zero Jank)**: Execução assíncrona desacoplada da thread principal de interface por canais circulares lock-free (SPSC/MPSC) em ring-buffers contíguos, preservando a fluidez em telas LTPO/ProMotion a 120Hz.

---

## 2. Motivação (Motivation)

O desenvolvimento multiplataforma contemporâneo para dispositivos móveis está fragmentado entre abordagens que sofrem de gargalos arquiteturais fundamentais:

| Plataforma / Abordagem | Arquitetura | Falhas Críticas de Mercado |
| :--- | :--- | :--- |
| **Flutter / Dart** | Engine próprio (Skia / Impeller) com renderização gráfica customizada em canvas GPU. | **Perda de Fidelidade do SO**: Não utiliza widgets nativos; acessibilidade (TalkBack/VoiceOver), novos estilos de interface, seleção de texto nativa e teclados virtuais comportam-se de maneira sutilmente defasada. **Shader Jank**: Mesmo com Impeller, a pré-compilação de shaders não elimina atrasos de inicialização. **Serialização FFI**: A comunicação entre isolates Dart e platform channels exige cópias e marshaling via `StandardMessageCodec`. |
| **React Native (Hermes + Fabric)** | JavaScript/TypeScript interpretado/bytecode com ponte C++ (JSI) e views nativas. | **Sobrecarga de Conversão JSI**: Conversão contínua de tipos primitivos e estruturas entre o heap JavaScript e C++. **Pausas de GC**: O garbage collector do Hermes disputa ciclos de CPU na thread de renderização, gerando quedas ocasionais de quadros durante listas longas ou animações simultâneas. |
| **Kotlin Multiplatform (KMP)** | Lógica compartilhada compilada para JVM (Android) e Kotlin/Native via LLVM (iOS). | **Choque ARC vs Tracing GC**: No iOS, o Kotlin/Native roda um garbage collector em background que disputa recursos com o Swift ARC. Pontes de objetos geram ciclos de retenção e overhead de alocação de invólucros (*wrappers*). **Ponte C-Header Frágil**: A geração de bindings para Swift passa por cabeçalhos Objective-C legados, perdendo enums com valores associados e tipos modernos de Swift. **Tempos de Compilação Longos**: O pipeline LLVM do Kotlin/Native é excessivamente lento em iterações locais. |
| **Rust Mobile (UniFFI / flutter_rust_bridge)** | Código nativo sem GC exportado via geradores de bindings externos. | **Cópia de Memória em Toda Chamada**: UniFFI serializa estruturas em arrays de bytes usando formatos binários intermediários, forçando serialização e deserialização em ambos os lados da FFI. **Cerimônia de Integração**: Exige pipelines manuais com scripts complexos de Gradle, CMake e Xcode, sem integração incremental no compilador. |

### Por que o Arandu Consegue Superar o Estado da Arte?

O compilador Arandu foi concebido com quatro pilares que se encaixam com precisão matemática nas demandas dos sistemas operacionais móveis modernos:

1. **Ausência de Tracing GC**: O Arandu utiliza confinamento estático de ciclo de vida (OSSA), empréstimos estritos e arenas geracionais (`GenRef`). Ao rodar no iOS, seu código nativo desfaz recursos instantaneamente sem disparar nenhum ciclo de GC que interrompa threads. Ao rodar no Android, a alocação externa em memória direta desacarrega o coletor de lixo da JVM.
2. **DataLayout Parametrizável Nativo**: O compilador já possui suporte formal e testado a layouts de 32 e 64 bits (`DataLayout`), facilitando a emissão precisa de tipos alinhados para AArch64 (ARMv8-A e ARMv9-A).
3. **Compilação Incremental Sub-Segundo**: Ao invés de aguardar minutos pela compilação LLVM a cada pequena alteração de código móvel, a integração Salsa ([RFC 0005](0005-incremental-query-system-salsa.md)) somada ao codegen particionado da [RFC 0011](0011-incremental-partitioned-aot-and-in-process-linker.md) gera novos binários e bindings atualizados em milissegundos para o simulador ou dispositivo de teste.
4. **Bindings Idiomáticos de Primeira Classe**: A emissão dos wrappers Kotlin e Swift é uma query de compilação interna ao Arandu, e não uma ferramenta de terceiros baseada em regex ou parsers paralelos de texto.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

O desenvolvedor Arandu escreve módulos de negócio, estruturas de dados e operações semânticas anotando os pontos de exposição pública com atributos canônicos `@mobile`:

### 3.1. Declarando um Módulo de Negócios Mobile

```arandu
// src/services/analytics.ar
import std.core.collections.Vec;
import std.core.slice.Slice;

@mobile
struct TelemetryRecord {
    id: u64,
    timestamp: u64,
    latitude: f64,
    longitude: f64,
    payload_tag: u32,
}

@mobile
struct EngineMetrics {
    processed_count: u32,
    p99_latency_us: f32,
}

@mobile
interface TelemetrySink {
    func on_batch_ready(count: u32): void;
}

@mobile
class TelemetryProcessor {
    records: Vec<TelemetryRecord>,
    metrics: EngineMetrics,

    pub func new(): TelemetryProcessor {
        return TelemetryProcessor {
            records: Vec.new(),
            metrics: EngineMetrics { processed_count: 0, p99_latency_us: 0.0 },
        };
    }

    // Chamada síncrona de alto desempenho: Zero cópia através de slice contígua
    @mobile
    pub func ingest_batch(self: mut Self, batch: []TelemetryRecord): u32 {
        for record in batch {
            self.records.push(record);
        }
        self.metrics.processed_count = self.metrics.processed_count + batch.len() as u32;
        return batch.len() as u32;
    }

    // Chamada assíncrona: Executada fora da UI Thread sem travar o app
    @mobile(async)
    pub func process_and_compress(self: ref Self): []u8 {
        // Algoritmo intensivo de compressão executado no runtime de paralelismo estruturado do Arandu
        return self.compress_records();
    }
}
```

### 3.2. Consumindo no Android (Kotlin + Jetpack Compose)

O compilador Arandu gera automaticamente o pacote `io.arandu.generated` contendo interfaces idiomáticas para Kotlin, integrando-se perfeitamente com Coroutines e Compose:

```kotlin
// Android: UI nativa fluida em Jetpack Compose
package com.myapp.ui

import androidx.compose.runtime.*
import androidx.compose.material3.*
import io.arandu.generated.TelemetryProcessor
import io.arandu.generated.TelemetryRecord
import kotlinx.coroutines.launch

@Composable
fun TelemetryScreen(processor: TelemetryProcessor) {
    val coroutineScope = rememberCoroutineScope()
    var isProcessing by remember { mutableStateOf(false) }
    var compressedSize by remember { mutableStateOf<Int?>(null) }

    Button(
        onClick = {
            coroutineScope.launch {
                isProcessing = true
                // Invoca a função assíncrona do Arandu diretamente como suspend fun:
                // Nenhum frame é perdido na MainLooper/UI thread!
                val compressedBytes = processor.processAndCompress()
                compressedSize = compressedBytes.size
                isProcessing = false
            }
        },
        enabled = !isProcessing
    ) {
        Text(if (isProcessing) "Processando..." else "Comprimir Métricas")
    }

    compressedSize?.let { size ->
        Text("Bytes gerados (Zero-Copy): $size")
    }
}
```

### 3.3. Consumindo no iOS (Swift + SwiftUI)

No iOS, o compilador sintetiza um pacote Swift que interage diretamente com Swift Concurrency (`async/await`) e o sistema de observabilidade `@Observable`:

```swift
// iOS: UI nativa declarativa em SwiftUI
import SwiftUI
import AranduMobileCore

@Observable
final class TelemetryViewModel {
    private let processor = AranduTelemetryProcessor()
    var isProcessing = false
    var compressedSize: Int? = nil

    func process() {
        Task {
            isProcessing = true
            // Chamada assíncrona nativa Swift mapeada diretamente da corrotina Arandu:
            let bytes = await processor.processAndCompress()
            self.compressedSize = bytes.count
            self.isProcessing = false
        }
    }
}

struct TelemetryView: View {
    @State private var viewModel = TelemetryViewModel()

    var body: some View {
        VStack(spacing: 20) {
            Button("Comprimir Métricas") {
                viewModel.process()
            }
            .disabled(viewModel.isProcessing)

            if let size = viewModel.compressedSize {
                Text("Bytes gerados (Zero-Copy): \(size)")
            }
        }
    }
}
```

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

### 4.1. Visão Geral da Arquitetura e Fluxo de Dados

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│                      Arandu Mobile Compiler Pipeline                         │
├─────────────────────────────────────────────────────────────────────────────┤
│ Código-Fonte Arandu (.ar)                                                   │
│     │                                                                       │
│     ▼                                                                       │
│ CST / AST ──► Typeck & Semantics ──► AMIR (SSA/OSSA)                        │
│                                           │                                 │
│                ┌──────────────────────────┴──────────────────────────┐      │
│                ▼                                                     ▼      │
│   [arandu_mobile_bindgen] (Salsa)                        [Cranelift / C]    │
│        ├── Kotlin Stubs (.kt)                                │              │
│        │     (DirectByteBuffer, @CriticalNative)             ▼              │
│        ├── Swift Stubs (.swift)                       Código Nativo AArch64  │
│        │     (UnsafeRawBufferPointer, async/await)     ├── libcore.so (ELF) │
│        └── C ABI Headers (.h)                          └── libcore.dylib    │
└──────────────────────────────────────────────────────────────┼──────────────┘
                                                               │
                     Zero-Copy FFI Boundary                    │
     ┌─────────────────────────────────────────────────────────┴──────────────┐
     ▼                                                                        ▼
┌──────────────────────────────┐                         ┌────────────────────────────────┐
│      Android Runtime (ART)   │                         │            Apple iOS           │
├──────────────────────────────┤                         ├────────────────────────────────┤
│ • Jetpack Compose UI (120Hz) │                         │ • SwiftUI / Metal UI (120Hz)   │
│ • Kotlin Coroutines Engine   │                         │ • Swift Concurrency Actors     │
│ • DirectByteBuffer Off-Heap  │                         │ • UnsafeRawBufferPointer View  │
│ • JNI CriticalNative Shims   │                         │ • Zero-GC Tracing Bridge       │
└──────────────────────────────┘                         └────────────────────────────────┘
```

### 4.2. Plano de Dados Zero-Copy (Zero-Copy Data Plane)

A serialização convencional (JSON, Protocol Buffers, MessagePack, ou o encoder binário do UniFFI) incorre em custos proibitivos de CPU e memória em dispositivos móveis:
- Uma estrutura com 10.000 registros gera 10.000 pequenas alocações no heap gerenciado (disparando o coletor de lixo).
- Duplicação contínua da memória durante o marshaling.

#### Mecanismo Zero-Copy do Arandu:
1. **Representação em Memória Contígua**: O tipo `[]T` em Arandu é representado internamente pela tripla C-ABI canônica: `{ ptr: *const T, len: usize, cap: usize }`.
2. **Projeção para Android via `DirectByteBuffer`**:
   - Para transferências de entrada (`host -> arandu`), o Kotlin aloca um buffer fora do heap via `ByteBuffer.allocateDirect(bytes)`. O ponteiro bruto obtido via `GetDirectBufferAddress` no C JNI é passado diretamente ao Arandu como um `[]T`.
   - Para transferências de saída (`arandu -> host`), o Arandu aloca a fatia em sua arena local e devolve o ponteiro bruto. O stub Kotlin envolve o ponteiro em uma instância leve de `DirectByteBuffer` usando o método JNI interno `NewDirectByteBuffer(env, ptr, len)`. Zero bytes são copiados.
3. **Projeção para Swift via `UnsafeRawBufferPointer`**:
   - Em Swift, a estrutura contígua é consumida diretamente como `UnsafeBufferPointer<T>` ou `Data(bytesNoCopy:deallocator:)`. O deallocator é configurado para invocar a função de liberação no Arandu (`ar_rt_drop_slice`), respeitando a posse de memória sem intermediários.

### 4.3. Otimização JNI: `@CriticalNative` e `@FastNative`

Em chamadas JNI padrão, o Android Runtime (ART) executa uma rotina dispendiosa:
- Fixação (*pinning*) de threads e criação de frames de transição de stack JNI.
- Checagem de exceções e sincronização com o estado do Garbage Collector.

O Arandu emite os wrappers JNI utilizando os modificadores de alto desempenho do ART:

```c
// Stubs gerados em C pelo Arandu para a ponte Android JNI
#include <jni.h>

// Chamada CriticalNative: ART salta a passagem de JNIEnv* e jclass,
// eliminando a barreira de stack e executando em ~2ns.
JNIEXPORT jint JNICALL
Java_io_arandu_generated_TelemetryProcessor_ar_1ingest_1batch_1critical(
    jlong native_handle,
    jlong buffer_ptr,
    jint count
) {
    ArTelemetryProcessor* self = (ArTelemetryProcessor*)native_handle;
    ArRecordSlice slice = { .ptr = (const ArRecord*)buffer_ptr, .len = (size_t)count };
    return (jint)ar_telemetry_ingest_batch(self, slice);
}
```

No lado Kotlin, a declaração é anotada com `@dalvik.annotation.optimization.CriticalNative`:

```kotlin
package io.arandu.generated

class TelemetryProcessor internal constructor(private val handle: Long) {
    @dalvik.annotation.optimization.CriticalNative
    private external fun ar_ingest_batch_critical(handle: Long, bufferPtr: Long, count: Int): Int
}
```

### 4.4. Modelo de Concorrência e Confinamento de UI Threads

A causa número um de *jank* (queda de frames abaixo de 60/120 FPS) em aplicações móveis é o bloqueio acidental da thread principal (`MainLooper` no Android ou `RunLoop.main` no iOS) por operações computacionais ou sincronizações com locks.

O runtime móvel do Arandu impõe a seguinte invariante estrutural:
1. **Thread Confinement**: Todas as funções anotadas com `@mobile(async)` têm sua execução automaticamente despachada para o worker pool nativo do Arandu ([RFC 0003](0003-structured-parallelism.md)).
2. **Sincronização Não-Bloqueante de Estado via Ring-Buffer**:
   - As atualizações de estado emitidas pelo Arandu em direção à interface fluem através de um canal circular *Single-Producer Single-Consumer* (SPSC) lock-free implementado em memória linear compartilhada com semântica atômica (`Acquire/Release`).
   - A thread de UI consome os deltas de estado sem jamais sofrer contenção de mutex ou esperar por operações de I/O.

### 4.5. Integração com o Motor Incremental Salsa (`arandu_mobile_bindgen`)

A geração de código Kotlin e Swift é uma extensão de query Salsa pura:

```rust
// arandu_query / arandu_middle: Assinatura da query pura de geração de bindings
#[salsa::tracked]
pub fn mobile_bindings_for_file(
    db: &dyn SourceDatabase,
    file_id: FileId,
    target_platform: MobilePlatform,
) -> Arc<MobileBindingsArtifact> {
    // 1. Obtém o HIR semântico já tipado e verificado
    let hir = db.hir_file(file_id);

    // 2. Extrai tipos anotados com @mobile
    let mobile_types = collect_mobile_exports(hir);

    // 3. Emite os códigos-fonte Kotlin/Swift em buffers de string determinísticos
    emit_mobile_bindings(&mobile_types, target_platform)
}
```

Como a query depende estritamente da árvore semântica exportada (`exported_symbols` e HIR limpo), qualquer modificação puramente interna ao corpo de uma função não invalida os arquivos de bindings gerados. Isso viabiliza que o compilador mantenha os arquivos `.kt` e `.swift` intactos, economizando compilações nos lados do Xcode e Gradle.

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

### 5.1. Preservação dos Invariantes Arquiteturais do Arandu

1. **Early-Cutoff de Queries Salsa**: A query `mobile_bindings_for_file` utiliza hashing estrutural. Adições de linhas, comentários ou refatorações de código privado em arquivos Arandu produzem a mesma saída hash-estável, impedindo recompilações em cascata no Gradle ou Xcode.
2. **Pureza e Determinismo Estritos**: A geração de bindings é 100% pura: sem I/O de disco, sem `fs::write` no hot-path de análise e sem dependência de variáveis de ambiente não rastreadas.
3. **Integridade de `SymbolId` e `TargetInfo`**: A exportação móvel respeita o layout exato de `TargetInfo::aarch64_apple_darwin()` e `TargetInfo::aarch64_linux_android()`. Alinhamentos de struct, tamanhos de ponteiro (64 bits) e preenchimento de campos obedecem estritamente às especificações da plataforma de destino.
4. **Recuperação Resiliente sem Pânico**: Declarações com uso incorreto de `@mobile` (por exemplo, exportação de tipos que contêm ponteiros crus sem garantia de confinamento) emitem diagnósticos formais (`N...` ou `T...`) com sugestões de correção estruturadas, sem nunca executar `unwrap()` ou `panic!`.

### 5.2. Desvantagens e Custos de Complexidade

* **Dependência do NDK e Xcode Command Line Tools**: Para a geração final dos binários compilados (`.so` e `.dylib`), o sistema depende de toolchains instaladas no host (Android NDK para compilação C/Cranelift de Android e `clang` com SDK da Apple para iOS).
* **Gerenciamento de Fatias no Swift**: Slices contíguas expostas ao Swift sem cópia exigem que o desenvolvedor iOS compreenda o ciclo de vida do objeto pai, a fim de evitar acesso após a liberação (*use-after-free*) em código que capture ponteiros crus.

---

## 6. Racional e Alternativas (Rationale & Alternatives)

### 6.1. Racional: Por que Headless Core + UI Nativa?

* **Fidelidade e Experiência do Usuário (UX)**: Motores que desenham seus próprios componentes (como Flutter) frequentemente falham no suporte completo a novas diretrizes de acessibilidade, tipografia dinâmica, transições de gestos nativas e recursos específicos de novíssimas versões de sistemas operacionais (ex: Dynamic Island no iOS, Predictive Back Gesture no Android).
* **Consumo de Bateria e Memória**: Carregar uma engine gráfica inteira (ex: Skia/Impeller) consome de 15MB a 30MB extras de memória RAM e impõe overhead constante de GPU. Delegar a UI aos toolkits nativos otimizados da plataforma (Jetpack Compose e SwiftUI) permite que o sistema operacional aplique técnicas nativas de economia de energia e aceleração por hardware.

### 6.2. Alternativas Rejeitadas

1. **Serialização Baseada em JSON / Protobuf sobre FFI**:
   - *Motivo da rejeição*: Incorre em alto consumo de bateria, desperdício de ciclos de CPU em encoders/decoders e sobrecarga massiva no garbage collector dos dispositivos móveis devido a alocações efêmeras.
2. **Geração de Bindings Exclusivamente em Objective-C**:
   - *Motivo da rejeição*: Embora Objective-C seja a ponte tradicional da Apple, ela impõe impedância semântica severa com o Swift moderno: ausência de enums associados, perda de segurança estrita de ponteiros e impossibilidade de mapear nativamente corrotinas para `async/await`.
3. **Embutir um Garbage Collector Próprio (Estilo Go / Kotlin Native Histórico)**:
   - *Motivo da rejeição*: A existência de dois GCs rodando concorrentemente no mesmo processo (ART GC + GC do Arandu no Android; Swift ARC + GC do Arandu no iOS) causa degradação imprevisível de performance, pausas visíveis de renderização (*frame drops*) e vazamento crônico de memória em ciclos de referências cruzadas.

---

## 7. Arte Prévia (Prior Art)

A arquitetura proposta apoia-se em estudos formais e implementações industriais de vanguarda:

1. **Dart FFI & Android ART CriticalNative Optimization**:
   - A documentação de engenharia do Android Open Source Project (AOSP) documenta que anotações `@CriticalNative` reduzem a latência de invocação de ~20ns para ~2.5ns por chamada em processadores ARM64, tornando o custo de travessia imperceptível.
2. **Swift-C++ Interoperability (Swift Evolution SE-0382, SE-0400)**:
   - Demonstrou a viabilidade de transitar coleções contíguas e tipos de valor entre C++ e Swift sem empacotadores de referência dinâmica, servindo de fundação para o mapeamento de slices do Arandu.
3. **Pesquisa Acadêmica sobre Jank e Garbage Collection em Dispositivos Móveis**:
   - *T. Yang et al., "Eliminating Garbage Collection Pauses in Real-Time Mobile Applications" (OOPSLA)*: Demonstrou que pausas de GC superiores a 5ms provocam perda direta da janela de renderização de 120Hz (8.33ms), justificando a eliminação de tracing GC no núcleo de execução.
4. **UniFFI (Mozilla) e flutter_rust_bridge**:
   - Pioneiros na geração automatizada de bindings nativos para linguagens modernas a partir de especificações de interface, mas limitados pelo modelo de cópia serializada de dados que o Arandu agora supera via Zero-Copy Direct Memory.

---

## 8. Questões em Aberto (Unresolved Questions)

1. **Tratamento de Pânicos e Exceções Através da FFI**:
   - *Status*: Proposta inicial converte erros recuperáveis em tipos `Result<T, E>` mapeados para exceções nativas (`AranduException` em Kotlin e `throws` em Swift). Comportamentos para pânicos não tratados (ICE) devem abortar de maneira segura ou capturar o frame antes de retornar um código de status de erro ao host?
2. **Estratégia de Empacotamento de Binários (Distribution)**:
   - A automação de build via `arandu package --target mobile` deve emitir diretamente arquivos `.aar` (Android Archive com Maven POM) e `.xcframework` prontos para Swift Package Manager (SPM) ou CocoaPods?

---

## 9. Possibilidades Futuras (Future Possibilities)

1. **Hot-Reloading Dinâmico de Lógica de Negócios via JIT em Emuladores**:
   - Aproveitar o compilador JIT Cranelift existente no Arandu para injetar novas versões de código compilado diretamente na memória do simulador iOS ou emulador Android durante a sessão de debug, sem reiniciar o aplicativo móvel.
2. **Extensão para Plataformas Vestíveis e Embarcadas Leves**:
   - Compilação direta de módulos ultraleves para Apple Watch (watchOS) e relógios inteligentes Android (WearOS), mantendo consumo de energia e tamanho de binário em níveis mínimos.
3. **Gerador de Modelos de Estado MVI Completo**:
   - Suporte nativo a geração de `Reducers` de arquitetura MVI (Model-View-Intent), onde o Arandu emite automaticamente tipos de estado consumíveis por `@Observable` (SwiftUI) e `StateFlow` (Jetpack Compose) com diffing automático de alterações.
