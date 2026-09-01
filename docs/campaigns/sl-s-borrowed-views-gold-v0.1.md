# Campanha SL_S — Borrowed Views Gold

Status: **ativa**  
Proprietários: `arandu_middle`, `arandu_typeck`, `arandu_mir`, `arandu_query`,
backends, CLI/LSP e `std.core`  
Entrada: auditoria AUD.0–AUD.5 concluída com classificação **parcial**  
Saída: borrowed views estruturais seguras e utilizáveis em `Slice` e `String`

Este é um plano temporário de campanha. Ao atingir os critérios Gold, as decisões
e evidências úteis serão consolidadas na arquitetura da stdlib e no roadmap
mestre, e este arquivo será removido.

## 1. Objetivo e limites

Completar a parte ainda parcial do contrato de borrowed views sem introduzir:

- sintaxe explícita de lifetimes;
- `*`, `&` ou ponteiros crus na superfície segura;
- RC/ARC, heap promotion ou GenRef como substituto de prova estática;
- heurísticas baseadas em nomes como `Vec.push` ou `String.reserve`;
- um segundo borrow checker separado do AMIR/OSSA;
- efeitos de filesystem ou estado global dentro de queries Salsa.

A sintaxe pública permanece uniforme:

```aru
func ler(valor: ref Mensagem): ref str
func editar(valor: mut ref Mensagem): mut ref str
```

`T`/`own T`, `ref T` e `mut ref T` continuam sendo as únicas formas públicas.
Tipos que contêm referências seguras passam a herdar restrições de escape e
exclusividade estruturalmente; não será adicionada agora uma anotação pública
equivalente a lifetime ou `~Escapable`.

## 2. Decisões validadas pela pesquisa

1. **A dependência pertence ao contrato semântico exportado, não ao endereço cru.**
   O Rust permite elisão apenas em relações não ambíguas e exige tornar a
   relação explícita nos demais casos. Arandu preservará a sintaxe simples: a
   assinatura restringe relações legais, o OSSA demonstra quais origens chegam
   ao retorno e o summary publica apenas esse resultado.
2. **Borrowed view é não escapável por transitividade.** Um `Option<ref T>`,
   `Result<ref T, E>`, tuple, enum ou struct que contenha uma referência segura
   também carrega a dependência. Isso evita o buraco clássico de esconder um
   empréstimo dentro de um agregado escapável.
3. **Múltiplas origens demonstradas usam união conservadora.** Se caminhos
   alcançáveis puderem retornar `a` ou `b`, o chamador considera ambos
   emprestados enquanto o resultado estiver vivo. Um parâmetro apenas
   compatível pelo tipo não entra no conjunto se o fluxo não o propaga ao
   retorno.
4. **Mutação é bloqueada pelo owner, não pelo nome da operação.** Uma chamada
   que requer `mut ref` conflita com qualquer view compartilhada ainda viva do
   mesmo owner; assim `push`, `reserve`, `clear` e equivalentes são cobertos
   pelo contrato geral de exclusividade.
5. **Views seguras são zero-overhead no runtime, mas não “raw”.** `Slice<T>`
   pode manter ABI `ptr + len`; sua dependência vive no tipo/IR e é apagada após
   a verificação. Construção a partir de ponteiro cru permanece `unsafe` e não
   ganha segurança retroativamente.
6. **As relações exportadas são canônicas e limitadas.** Caminhos e conjuntos
   de origens serão ordenados, sem `TypeId` internado ou `HashMap` na forma
   hash-estável. Limites explícitos de profundidade e quantidade impedem
   explosão patológica; excedê-los falha fechado com diagnóstico.

Essas escolhas seguem a separação usada por Polonius entre origem, loan e
pontos em que o loan está vivo; e incorporam as lições das propostas de
nonescapable values e `Span` do Swift: containers propagam não-escapabilidade,
views impedem mutação do owner e `Optional`/`Result` precisam participar do
modelo, não contorná-lo.

## 3. Arquitetura-alvo

### 3.1 Contrato público estável

O resumo escalar encontrado no início da campanha:

```rust
ReturnBorrowSummary { parameter_index, kind }
```

será generalizado conceitualmente para:

```text
ReturnBorrowSummary
└── dependencies[]
    ├── result_path       # raiz, campo, tuple slot ou payload de variante
    ├── argument_indices  # conjunto ordenado e sem duplicatas
    └── kind              # shared ou exclusive
```

O nome final dos tipos Rust pode mudar durante a implementação, mas estes
invariantes não:

- o resumo descreve somente a superfície exportada;
- não contém `TypeId`, spans ou IDs incidentais;
- é ordenável, hash-estável e independente de `HashMap`;
- uma mudança apenas no corpo não invalida importadores se o contrato não muda;
- uma saída estrutural sem origem segura demonstrável é rejeitada antes do
  backend.

`result_path` identifica onde a referência está no valor retornado. A origem
de segurança é o owner raiz do argumento, e não um endereço intermediário.
O fluxo tipado determina quais origens realmente alcançam cada caminho do
resultado. Branches fazem união; calls aplicam o summary do callee; ciclos são
resolvidos por SCC/worklist até fixpoint. A assinatura continua sendo o limite
de validade, não um substituto conservador para a análise corporal.

### 3.2 Propriedades estruturais dos tipos

O `TypeInterner`/contrato de tipos deverá responder, de forma pura e memoizável:

- se o tipo contém borrow;
- se contém borrow exclusivo;
- se pode escapar;
- se pode ser copiado ou apenas movido;
- quais caminhos de resultado carregam dependências.

Regras mínimas:

- `ref T`: não escapável e copiável localmente;
- `mut ref T`: não escapável e não copiável; só pode ser movido/reborrowed;
- agregado com `ref`: não escapável;
- agregado com `mut ref`: não escapável e não copiável;
- generic carrier herda as propriedades de seus argumentos;
- raw pointer não cria nem recupera proveniência segura.

### 3.3 Fluxo pelo compilador

```text
CST/AST type syntax
      ↓
typeck: propriedades estruturais + relações legalmente possíveis
      ↓
AMIR/OSSA: transferência simbólica de origens pelo CFG
      ↓
arandu_mir: solve SCC/fixpoint + resumo canônico por item
      ↓
arandu_query: HashEq/early-cutoff sobre o resumo, não sobre o corpo inteiro
      ↓
AMIR enriquecida: calls, aggregates, loans/holders e escape analysis
      ↓
C/Cranelift: mesmo ABI; nenhum metadado de borrow em runtime
```

## 4. Plano de implementação

O trabalho será entregue em **três marcos coesos**, não em micro-patches. Cada
marco deve terminar com código, regressões e documentação sincronizados.

### Marco BV.1 — Contrato estrutural e incrementalidade

- [x] Substituir o resumo escalar por dependências estruturais canônicas.
- [x] Implementar inspeção recursiva de `ref`/`mut ref` em `Option`, `Result`,
      tuple, struct e enum, com proteção contra tipos recursivos.
- [x] Construir a transferência simbólica de origens sobre o fluxo tipado; a
      assinatura restringe relações legais e receiver/método é o formal zero.
- [x] Resolver chamadas recursivas por SCC/worklist determinística até fixpoint;
      limite excedido ou relação irrepresentável deve falhar fechado.
- [x] Rejeitar retorno seguro sem nenhuma origem formal demonstrável; retorno
      de local continua O010.
- [x] Propagar o novo resumo por `TypeInfo`, símbolos exportados, imports e
      especializações genéricas.
- [x] Atualizar o stable hash e provar early-cutoff: alteração corporal não
      invalida chamadores; alteração do resumo invalida somente dependentes.
- [x] Definir limites determinísticos de profundidade/quantidade e diagnóstico
      fail-closed para contrato estrutural excessivo.

**Saída BV.1 concluída:** o corpo demonstra a relação uma vez; callers consomem
somente a interface canônica, sem depender do corpo, do backend ou de IDs
instáveis. Os limites publicados são 32 segmentos por caminho e 256 folhas de
borrow por tipo; excedê-los não trunca a prova e termina em O010 fail-closed.

Implementação principal: `arandu_middle::types::borrow`,
`TypeInfo::borrow_paths`, `arandu_mir::borrow_interface`,
`lower_to_amir_with_interfaces` e a query estreita `borrow_interfaces`.
Regressões cobrem união de branches sem falso candidato, composição de calls,
recursão, import, especialização genérica, `Option<ref T>` e cutoff incremental.

### Marco BV.2 — Propagação OSSA, agregados e controle de fluxo

- [x] Generalizar `CallBorrowDependency` para múltiplos caminhos e origens
      (entregue como pré-requisito estrutural da BV.1).
- [x] Propagar holders por `Use`, `Load`, `Store`, aggregate construction,
      field/tuple projection, enum payload, match/destructuring e reborrow.
- [x] Unir dependências em block arguments, branches e loops até fixpoint.
- [x] Atualizar todos os visitors compartilhados: liveness, borrow facts, move
      checker, escape analysis, DCE, CFG transforms, pretty printer e auditor.
- [x] Tornar aggregates com borrow não escapáveis; proibir global/static,
      armazenamento em heap não provado e captura por closure escapável.
- [x] Permitir closure não escapável somente quando o próprio contrato de
      closure puder provar a janela; até lá, rejeitar em vez de promover.
- [x] Manter a regra de suspensão: borrow absoluto cru não atravessa `await`;
      somente a representação relativa já provada por A3 pode atravessar.
- [x] Provar que qualquer chamada `mut ref` conflita com views vivas do owner,
      incluindo mutações que podem realocar storage.

**Saída BV.2:** o borrow não desaparece quando é copiado, armazenado, colocado
em carrier ou unido por CFG, e nenhuma forma ainda não modelada chega ao backend.

**BV.2 concluída:** `borrow_facts` mantém caminhos ordenados por loan para temps
e places, compõe `ReturnBorrowSummary` em calls e executa um dataflow forward
para carriers locais. Stores projetados removem somente o subcaminho escrito;
joins unem fatos por caminho e loops convergem sobre domínio finito. Liveness,
borrow checker, escape analysis e `borrow_audit` consomem a mesma relação. A
checagem de `Suspend` inspeciona carriers, não apenas temps cujo tipo de topo é
`ref`. `ref T` permanece copiável; `mut ref T` e qualquer carrier estrutural que
o contenha são move-only. Como closures ainda não são uma construção pública da
linguagem, nenhuma captura recebe exceção implícita: a política atual é
fail-closed até existir um contrato não escapável próprio.

Regressões executáveis cobrem tuple/field projection, store/load projetado,
enum payload, resumo estrutural de call, overwrite, block arguments, loops,
auditoria determinística, exclusividade e borrow absoluto escondido em aggregate
atravessando `await`. A suíte completa também exercita os dois backends, CLI,
queries Salsa e LSP; proveniência continua exclusivamente compile-time e não
altera layout ou ABI.

#### Plano de execução da BV.2

A BV.2 será implementada e validada como um marco completo. As divisões abaixo
são fronteiras internas de revisão e commits, não entregas parciais promovíveis.

1. **BV.2-A — Domínio estrutural de holders.** Substituir a propagação global
   baseada apenas em `BitSet<TempId/LocalId>` por fatos que associem cada caminho
   estrutural de temp/place aos loans que ele carrega. Reutilizar
   `BorrowPath`/`BorrowKind`; não criar IDs de lifetime públicos nem metadados de
   runtime. `mut ref` e qualquer carrier que o contenha são move-only.
2. **BV.2-B — Transferência OSSA única.** Implementar uma transferência pura e
   exaustiva para `Use`, `Load`, `Store`, reborrow, tuple/array/struct/enum,
   projeções, payloads e calls. Construção prefixa caminhos; projeção remove o
   prefixo correspondente; overwrite mata apenas o subcaminho sobrescrito. O
   auditor e o borrow checker consomem os mesmos fatos em vez de manter duas
   interpretações do AMIR.
3. **BV.2-C — Dataflow por CFG.** Propagar os fatos por argumentos de
   `Goto`/`Branch`/`Suspend`, parâmetros de bloco, match e loops com worklist
   determinística até o menor ponto fixo. O join é união conservadora por
   caminho; liveness encerra loans quando nenhum holder daquele valor permanece
   vivo, sem prolongar um predecessor morto para outro branch.
4. **BV.2-D — Enforcements e escape.** Bloquear move, destroy e mutação do owner
   enquanto uma view incompatível estiver viva; uma chamada que recebe
   `mut ref` é uma mutação sem depender do nome da função. Rejeitar carriers com
   borrow em heap/global/captura escapável não provados, conversão de raw pointer
   para referência segura e borrow absoluto através de `Suspend`. Somente
   `RelativeBorrow` já validado pode cruzar suspensão.
5. **BV.2-E — Transformações e produto.** Tornar visitors e validators
   exaustivos, auditar antes/depois de O1/O2 e provar que DCE, SCCP,
   SimplifyCFG, SROA e GVN preservam dependências. Fechar regressões unitárias,
   CLI, Salsa/early-cutoff, LSP stale diagnostics e paridade C/Cranelift. Os
   backends continuam recebendo o mesmo layout: toda proveniência é apagada
   somente depois da validação.

Critérios de aceite específicos:

- nenhum temp/place de tipo que contém borrow chega à validação sem holder
  estrutural conhecido ou origem externa formal explícita;
- overwrite de um campo não apaga loans de campos irmãos, e projeção não atribui
  ao resultado loans de outros campos;
- joins e loops convergem de forma byte-determinística e independem da ordem de
  `HashMap`;
- qualquer forma AMIR não modelada falha fechada antes dos backends;
- uma edição corporal que preserva `ReturnBorrowSummary` continua cortando a
  invalidação de importadores no Salsa;
- a matriz de carriers/CFG/acesso/exclusividade desta campanha fica integralmente
  verde em Windows e nos runners nativos Linux/macOS.

### Marco BV.3 — APIs públicas, backends e promoção Gold

- [x] Tornar `Slice<T>` uma borrowed view segura no sistema de tipos, mantendo
      ABI `ptr + len` e construção raw confinada a `unsafe`.
- [x] Publicar `Slice.get -> Option<ref T>`, `first`, `last`, subslice e
      iteração mínima sem cópia nem ponteiro seguro falsificado.
- [x] Publicar `String.asStr`, `String.asBytes` e `pushStr`; views vivas devem
      bloquear qualquer mutação/realloc do owner.
- [x] Cobrir `Option`, `Result`, tuple, struct e enum com shared e exclusive
      borrows, inclusive nested carriers e generic specialization.
- [x] Garantir paridade semântica e ABI nos backends C e Cranelift; metadados de
      lifetime não podem mudar layout ou aparecer no runtime.
- [x] Publicar corpus positivo/negativo CLI e incremental IDE para cada forma;
      diagnósticos mantêm código, labels, notes, hints e replacements.
- [ ] Exercitar targets nativos publicados 32/64 bits onde suportados pelo SDK,
      além de Windows, Linux e macOS da matriz oficial.
- [x] Rodar determinismo, fuzz regressions e testes de transformação AMIR para
      provar que otimizações não apagam dependências antes da checagem.
- [ ] Consolidar decisões/evidências em
      `arandu-stdlib-architecture-v0.1.md`, marcar `SL_S-Core` Gold apenas se
      todos os gates abaixo passarem e remover esta campanha.

**Implementação BV.3 concluída localmente:** `ArType::Slice` é um shared borrow
estrutural copiável; `SliceView`, `SliceSubslice` e `StrView` carregam owner na
AMIR e são apagados nos backends. Cranelift expande `[]T` em dois slots nas
assinaturas, block arguments e retornos, materializando descritores apenas
internamente. O backend C emite o mesmo layout e agora substitui campos
genéricos antes de formar lvalues, o que também corrige `Vec<T>` monomorfizado.
O runtime de `String.pushStr` é fallible e failure-atomic.

O corpus de produto cobre execução JIT, execução C nativa local, UTF-8,
subslice/element borrow, escape O010, conflito de realloc/mutação, diagnóstico
IDE incremental e layouts de dois slots com pointer width 32/64. Os itens ainda
abertos acima são evidências de promoção: matriz nativa/fuzz/determinismo, CI do
PR e remoção deste plano temporário — não implementação ausente da API BV.3.

**Saída BV.3:** usuários conseguem obter e compor views seguras de `Slice` e
`String`; código inseguro é rejeitado antes de C/Cranelift; os dois backends
executam o mesmo corpus válido.

## 5. Matriz obrigatória de regressão

| Eixo | Deve aceitar | Deve rejeitar |
| --- | --- | --- |
| origem | parâmetro único, receiver, forwarding, import, genérico | local retornado, raw pointer rebatizado como `ref` |
| carriers | `Option`, `Result`, tuple, struct, enum, nested | carrier escapando para global/heap não provado |
| CFG | branch, match, loop, block arguments | origem destruída em qualquer predecessor vivo |
| acesso | vários leitores e reborrow compartilhado | move/destroy/mutação do owner com view viva |
| exclusivo | mover `mut ref`, reborrow limitado | copiar/duplicar `mut ref`, alias exclusivo concorrente |
| calls | conjuntos de uma e múltiplas origens | dependência ausente ou índice fora da assinatura |
| async/closure | somente caso relativo/não escapável provado | borrow absoluto em `await`, closure escapável |
| incremental | body edit com cutoff; assinatura invalida dependentes | snapshot stale publicado ou resumo antigo reutilizado |
| backend | mesmo resultado em C e Cranelift | programa inválido alcançando emissão/JIT |

O corpus deve incluir também zero-length slices, limites, Unicode/UTF-8,
payload não trivial com `Drop`, tipo recursivo, nesting profundo limitado e
diagnósticos determinísticos em Windows/Linux/macOS.

## 6. Gates de aceitação

Nenhum marco é concluído apenas porque seus testes novos passam. Em cada marco:

1. `cargo fmt --all -- --check`
2. `cargo check --workspace --locked`
3. `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
4. `cargo test --workspace --locked`
5. `cargo run --locked -p xtask -- check-diag-docs`
6. `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked`
7. `cargo run --locked -p xtask -- check-architecture`
8. testes relevantes de `arandu_query`, CLI, stdio LSP, C e Cranelift.

Promoção Gold exige adicionalmente:

- zero caso **ausente** na matriz de carriers e controle de fluxo;
- zero caminho seguro baseado apenas em ponteiro cru;
- zero divergência C/Cranelift no corpus publicado;
- prova de 32/64 bits nos targets oficialmente distribuídos;
- relatório AUD atualizado de **parcial** para **suportado** sem ressalva que
  comprometa as APIs públicas;
- CI verde no `S0 / Gate` do PR.

## 7. Riscos e contenções

| Risco | Contenção |
| --- | --- |
| explosão de caminhos/origens | forma canônica limitada; excedente falha fechado |
| resumo demonstrado pelo corpo destrói cutoff | callers dependem somente do summary `HashEq`; body edit com summary igual corta propagação |
| `mut ref` escondido torna-se copiável | propriedade estrutural transitiva e teste de carriers |
| otimização apaga proveniência | visitors compartilhados + auditor antes/depois de transforms |
| stdlib expõe segurança falsa | APIs públicas entram somente no BV.3 |
| async/closure amplia escopo sem prova | política fail-closed até contrato próprio existir |
| metadado vaza para ABI | testes de layout e paridade; metadado compile-time only |

## 8. Referências primárias

- [Rust Reference — lifetime elision](https://doc.rust-lang.org/stable/reference/lifetime-elision.html)
- [RFC 2094 — non-lexical lifetimes](https://rust-lang.github.io/rfcs/2094-nll.html)
- [Polonius — loan analysis](https://rust-lang.github.io/polonius/rules/loans.html)
- [Swift SE-0446 — nonescapable types](https://github.com/swiftlang/swift-evolution/blob/main/proposals/0446-non-escapable.md)
- [Swift SE-0456 — `Span`-providing properties](https://github.com/swiftlang/swift-evolution/blob/main/proposals/0456-stdlib-span-properties.md)
- [Swift SE-0465 — nonescapable `Optional`/`Result`](https://github.com/swiftlang/swift-evolution/blob/main/proposals/0465-nonescapable-stdlib-primitives.md)
- [Swift SE-0519 — `Ref`/`MutableRef` e a limitação de uma única dependência](https://github.com/swiftlang/swift-evolution/blob/main/proposals/0519-ref-mutable-ref.md)
- [Cousot & Cousot, POPL 1977 — Abstract Interpretation](https://doi.org/10.1145/512950.512973)
- [Reps, Horwitz & Sagiv 1995 — IFDS interprocedural dataflow](https://doi.org/10.1145/199448.199462)
- [Tarjan 1972 — strongly connected components](https://doi.org/10.1137/0201010)
- [Salsa — red/green incremental algorithm](https://github.com/salsa-rs/salsa/blob/master/book/src/algorithm.md)
