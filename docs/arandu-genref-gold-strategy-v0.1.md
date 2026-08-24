# Estratégia GenRef Gold (v0.1)

**Status:** campanha aprovada; implementação Gold pendente

**Branch:** `codex/genref-stabilization`

**Escopo:** escape controlado, ABI, runtime, AMIR, backends, stdlib,
diagnósticos e provas de segurança

**Base existente:** `arandu-genref-abi-rfc-v0.1.md` registra o MVP atual

Este documento transforma o GenRef de uma demonstração funcional para um
fallback geracional estável. Ele não declara o MVP atual como Gold e preserva
a regra principal do Arandu: referências comprovadamente locais continuam
stack-first e sem custo geracional.

## 1. Resultado pretendido

```text
borrow provado localmente          -> referência direta, sem runtime check
escape permitido e representável  -> GenRef tipado + O004 + check obrigatório
escape proibido pela política      -> O004 como erro, sem lowering parcial
retorno de referência ao frame     -> O010, nunca mascarado por GenRef
unsafe/FFI                         -> fronteira explícita, sem promessa implícita
```

“Gold” significa a mesma semântica observável no Cranelift e no backend C, em
32 e 64 bits, inclusive em falha de alocação, handle stale, esgotamento de
geração, destruição e concorrência permitida.

## 2. Auditoria do estado atual

O MVP já oferece `EscapeKind::HeapStore`, O004/O010, operações Gen no AMIR,
round-trip de `i64` no JIT, emissão C básica, `GenArena<T>` na stdlib e a
política `--no-generational-fallback`. Ele é uma boa base, não o destino.

| Lacuna | Consequência | Classe |
| --- | --- | --- |
| runtime implícito restrito a `i64` | structs, enums e valores com destrutor não são promovidos | bloqueador |
| backend C usa 256 slots estáticos | programa válido aborta por limite artificial | bloqueador |
| geração `u32` usa wrap | handle antigo pode voltar a ser válido (ABA) | segurança |
| handle não prova a arena proprietária | chaves de arenas distintas podem colidir | segurança |
| `remove` stale retorna `0` no host | payload válido e falha tornam-se indistinguíveis | semântica |
| arena global com mutex no JIT | vida, contenção e envenenamento ficam implícitos | arquitetura |
| promoção reconhece padrões locais simples | aliases, CFG, projeções e destruição ficam incompletos | completude |
| não há contrato de drop glue | valores não triviais podem vazar ou sofrer double-drop | segurança |
| teste principal constrói AMIR à mão | o pipeline da linguagem não é provado | cobertura |

Ao fim da campanha, o RFC do MVP será marcado como substituído pelo contrato
Gold, sem apagar o registro da evolução.

## 3. Dependências auditadas no roadmap mestre

O status incompleto de um marco futuro não o torna automaticamente requisito
do GenRef. A campanha incorpora somente a fatia necessária para segurança e
paridade, preservando os demais marcos como evoluções independentes.

| Marco do roadmap | Relação com GenRef Gold | Decisão |
| --- | --- | --- |
| F2.0–F2.3 e M2 | base estática de borrow, escape e move | já implementada; pré-requisito satisfeito |
| `GenArena<T>` self-host | substitui a limitação `i64` e prova payload/drop genéricos | implementar em G1/G4 |
| ABI & Layout Stability (`ABI`) | GenRef precisa de layout e calling convention iguais entre targets/backends | implementar nesta campanha somente para GenRef; o marco ABI geral continua aberto |
| Panic & Error Model (`PAN`) | stale implícito, arena errada e corrupção precisam de trap determinístico | implementar nesta campanha somente para operações Gen; o modelo global continua aberto |
| Effect System (`A2`) | `noalloc` poderá proibir fallback e `nothrow` descrever falhas | não bloqueia; integrar depois sem enfraquecer `@NoFallback` |
| Memory Layout Optimization (`A4`) | poderá compactar `Option<GenRef<T>>` usando o zero inválido | não bloqueia; reservar zero agora evita quebra futura de ABI |
| Named multi-value ABI (`BC.5`) | pode melhorar passagem de agregados e do handle | não bloqueia; runtime pode usar ABI explícita/indireta correta antes dessa otimização |
| async runtime (`SL_R`) | arenas e handles compartilhados por tarefas exigirão política própria | não bloqueia; cruzamento não suportado deve ser rejeitado, nunca aceito parcialmente |
| Adaptive Monomorphization (`GEN`) | pode reduzir custo de payloads genéricos | não bloqueia; monomorfização existente basta para correção |
| LLVM / register allocation | podem otimizar checks e calling convention | não bloqueiam; Cranelift + C definem a paridade Gold atual |
| FFI Bindgen (`E3`) | poderá gerar wrappers opacos | não bloqueia; FFI de GenRef permanece fora da superfície estável |
| Allocation/escape linter (`E5`) | melhora sugestões e inspeção de O004 | desejável após G5, não requisito de segurança |
| self-hosting (`HOST`/`BOOT`) | trocará componentes host por Arandu | não bloqueia; o runtime deve ter contrato independente da linguagem de implementação |

Há também um requisito que o roadmap ainda não nomeia como marco próprio:
**drop glue para payload promovido**. Ele entra obrigatoriamente em G1/G2/G4.
Não será adiado para self-hosting porque, sem ele, `GenRef<T>` genérico pode
vazar recursos ou destruir o mesmo valor mais de uma vez.

## 4. Auditoria comparativa de falhas e trade-offs

“Falha” nesta tabela inclui vulnerabilidade, armadilha de API e trade-off
explicitamente aceito pelo projeto de referência. O objetivo não é declarar
outras implementações incorretas, mas impedir que uma escolha adequada para um
ECS ou biblioteca torne-se uma promessa de segurança mais forte no Arandu.

| Referência | Limite ou problema documentado | Decisão para o Arandu | Regressão obrigatória |
| --- | --- | --- | --- |
| Rust `slotmap` | a versão pode dar wrap depois de 2^31 reciclagens do mesmo slot e uma chave antiga pode coincidir; chaves usadas no mapa errado têm resultado seguro, porém não especificado | slot retirement sem wrap e identidade dinâmica de arena; apenas tipos de chave distintos não bastam | contador reduzido prova que stale nunca renasce; duas arenas com mesma sequência nunca se confundem |
| `generational-arena` | resolve ABA comum, mas o projeto foi arquivado e sua API não constitui contrato de runtime de linguagem | aproveitar o modelo seguro/falível, sem terceirizar ABI, drop ou manutenção crítica a uma crate arquivada | modelo de referência independente e differential test |
| Thunderdome | `insert_at` pode ressuscitar índice antigo; operações “by slot” ignoram geração deliberadamente | API segura da linguagem não expõe force-generation nem acesso by-slot; ferramentas internas exigem capability unsafe explícita | fuzz tenta fabricar/reativar chaves e só alcança falha definida |
| Bevy ECS | geração/representação evoluem entre releases e a API desencoraja fabricação manual | handle é opaco na superfície; packing e campos físicos não viram sintaxe pública | round-trip permitido; fabricação por bits arbitrários é rejeitada |
| Unity Entities | versão de 32 bits pode dar wrap; entidade pertence a um `World` | não aceitar risco probabilístico e incluir o domínio/arena na validação | overflow + cross-world/cross-arena em endurance |
| EnTT | versões compactas podem esgotar rapidamente em slot quente; IDs maiores apenas adiam o problema | largura maior pode reduzir frequência, mas retirement é a garantia | hot-slot recycle com largura pequena configurável em teste |
| Vale | prevê geração `u48`, aposentadoria no máximo, stacks por size class e dificuldade adicional para objetos inline | adotar retirement; não prometer size classes antes de medir; GenRef de subobjeto resolve o owner e uma projeção validada, nunca aponta para slot interior reciclável isolado | projeção/field após recycle falha antes de calcular endereço; owner vivo permite acesso correto |
| Swift `weak`/`unowned` | weak usa storage auxiliar; unowned seguro preserva metadados do objeto para trap determinístico, podendo prolongar memória; unsafe unowned volta a UAF | trap não pode ler memória já liberada, mas tombstones/metadados também não podem crescer sem limite; arena destruída deve invalidar por identidade estável e reclamável | destroy com handles restantes, reaproveitamento de endereço e auditoria de crescimento de metadados |
| CHERI/revogação | bounds/capabilities sozinhos não garantem temporal safety; revogação é mecanismo adicional de software/hardware | geração não será vendida como spatial safety, alias safety ou data-race safety; OSSA, bounds e política de threads continuam obrigatórios | testes independentes para stale, bounds, alias mutável e thread crossing |
| Fil-C | segurança completa exige checks amplos e controle da fronteira; sua solução usa GC concorrente e FFI restrita | GenRef não justifica alegar segurança completa da linguagem; Arandu mantém GC rejeitado e documenta a fronteira unsafe/FFI | corpus FFI prova que handles não são dereferenciáveis sem validação |

### Consequências adicionais

1. **Nada probabilístico é garantia.** Geração aleatória ou contador grande
   podem ser defesa adicional, mas não substituem retirement.
2. **Handle não é persistência.** `GenRef<T>` de runtime não é serializável,
   armazenável em disco nem válido entre processos por padrão. Persistência
   futura exige outro ID e protocolo.
3. **Arena destruída é um caso de stale.** A validação não pode dereferenciar
   um control block já liberado; também não pode reter um bloco zumbi por cada
   arena para sempre. G0 deve comparar registry geracional de arenas, epochs de
   processo e handles indiretos antes de congelar o layout.
4. **Projeção não cria ownership novo.** Um handle para `owner.field` mantém a
   identidade do owner e uma projeção tipada/bounds-checked. Reordenamento de
   layout, move do payload ou recycle não pode transformar offset antigo em
   acesso válido.
5. **APIs de recuperação são separadas das seguras.** Importar snapshot,
   restaurar índice ou forçar versão não pertence à API normal de GenArena.
6. **Falha determinística não significa abortar sempre.** A API explícita pode
   retornar `Result`/`Option`; apenas a dereferência implícita cujo tipo promete
   um valor usa o trap definido pelo modelo PAN.

## 5. Decisões arquiteturais

### G1 — Fallback híbrido, não ponteiro universal

OSSA e liveness tentam resolver a vida estaticamente. GenRef só aparece quando
a análise de escape provar a necessidade e a política permitir. Código sem
escape não paga check, indireção nem alocação geracional.

Um check só pode ser eliminado com prova explícita no CFG de que o slot não
pode ser removido, reciclado ou alterado concorrentemente no intervalo
dominado. Aparência de redundância não basta.

### G2 — Duas superfícies, um núcleo de invariantes

Existem dois produtos distintos:

1. fallback implícito do compilador, com arena e drop glue administrados pelo
   runtime;
2. `GenArena<T>` explícita da stdlib, administrada pelo programa.

Eles compartilham chave, reciclagem, falhas e testes de conformidade, mas não
fingem possuir o mesmo domínio de vida. O emissor C não deve conter uma terceira
implementação semântica independente.

### G3 — Handle tipado e vinculado à arena

O contrato lógico é:

```text
GenRef<T> = { arena_identity, slot_index, generation }
```

`T` pertence ao tipo de IR, mesmo sem ocupar bytes no handle. A identidade da
arena é verificada: usar uma chave em outra arena nunca acessa um valor por
coincidência de índice e geração.

O layout físico definitivo só será congelado depois de protótipos em i686,
x86_64 e aarch64. A preferência é uma representação explícita e portável, não
um `i64` escolhido pela conveniência do host. Compactação precisa preservar
identidade e domínio dos contadores.

### G4 — Esgotamento nunca vira ABA

Não haverá `wrapping_add`. Quando a geração seguinte não couber, o slot será
aposentado permanentemente e não voltará à free-list. Esgotamento de índice ou
capacidade vira falha definida; handle stale nunca renasce.

O zero será reservado para handle inválido. O primeiro handle válido não pode
ser `{ arena: 0, index: 0, generation: 0 }`; conversões e FFI não fabricam
validade silenciosamente.

### G5 — Falhas tipadas; violação é trap

- `try_insert` distingue overflow de capacidade e falha de alocação;
- `insert` pode ser conveniência abortiva documentada sobre `try_insert`;
- `get`/`get_mut` da arena explícita retornam ausência para stale;
- dereferência inserida pelo compilador produz trap determinístico com local e
  razão, nunca `0`, `nil` ou payload padrão;
- `remove` distingue removido, stale e arena errada sem sentinela de payload;
- invariantes internas quebradas viram ICE/abort controlado, não UB.

O004 continua mostrando onde e por que houve fallback. A grafia pública alvo é
`@NoFallback`; aliases antigos só existem durante a migração do contrato de
anotações.

### G6 — Payload genérico exige layout e drop glue

O runtime implícito armazena `T` usando `size`, `align` e drop glue derivados do
`DataLayout` do alvo. Struct não pode ser promovida por coerção para `i64`.

Cada slot tem estado `vacant`, `occupied` ou `retired`. A transição de occupied
executa drop exatamente uma vez. Mover o payload transfere ownership; destruir
a arena destrói apenas slots ainda ocupados. ZSTs, alinhamentos altos e
aritmética de capacidade verificada pertencem ao contrato.

### G7 — Concorrência não será prometida por acidente

GenRef é thread-confined por padrão. Compartilhamento só será seguro quando `T`
e a arena cumprirem o futuro contrato `Send`/`Sync`; até lá, cruzar threads é
rejeitado ou explicitamente unsafe. Uma trava global de processo não define a
semântica Gold. Sharding só entra depois de medição.

### G8 — FFI é uma fronteira explícita

`GenRef<T>` não é ponteiro bruto nem ABI C automaticamente estável. FFI usa
handle opaco e funções de validação, ou copia para memória da fronteira. A ABI
só será congelada depois de testes C/Arandu e política explícita de vida.

## 6. Campanha de implementação

Cada etapa deve terminar em PR revisável e deixar a `main` utilizável.

### G0 — Contrato e reprodução das falhas

- transformar as lacunas da auditoria em testes inicialmente negativos;
- criar modelos de estado para wrap, arena errada, arena destruída e projeção;
- medir workloads com zero, poucos e muitos escapes;
- gerar golden AMIR pelo pipeline real, não só IR manual;
- decidir a matriz de targets antes de congelar o layout.

**Saída:** suíte que demonstra os limites atuais sem mudar a semântica.

#### Registro de execução G0

- [x] modelo executável independente com identidade de arena, zero inválido,
  stale, arena destruída, retirement e drop único;
- [x] contador reduzido reproduz wrap/ABA do algoritmo MVP e prova que o modelo
  Gold aposenta slots e identidades de arena;
- [x] cross-arena e sentinela `0` foram reproduzidos como ambiguidades do MVP;
- [x] projeções validam o owner antes de aritmética checked de offset/bounds;
- [x] pipeline real prova custo estrutural zero para borrow local: nenhum
  `GenInsert`, `GenGet` ou `GenRemove` aparece no AMIR;
- [x] backend C tem teste de caracterização para o limite atual de 256 slots;
- [x] matriz inicial permanece i686, x86_64 e aarch64, com handle lógico de
  quatro domínios `u32`; o layout físico só congela após os protótipos G1/G5;
- [ ] baseline de poucos/muitos escapes: bloqueado pelo comportamento atual em
  que um aggregate store em código de superfície emite O004, mas o lowering
  devolve AMIR vazio. O teste G0 congela essa lacuna e será invertido em G4.

O G0 não usa AMIR construído à mão para esconder essa limitação. Até G4, apenas
o baseline sem escape é representativo do pipeline completo.

### G1 — Chave segura e arena explícita Gold

- introduzir `GenRef<T>` lógico e identidade de arena;
- reservar handle zero e aposentar slots no overflow;
- trocar sentinelas por resultados tipados;
- verificar crescimento, offsets e conversões;
- provar drop exatamente uma vez.

**Saída:** stdlib estável para arenas cruzadas, stale handles e payloads não
triviais.

#### Registro de execução G1

- [x] núcleo seguro de produção em `arandu_runtime::genref`, separado dos
  adapters ABI legados;
- [x] `ArenaId` e `GenRef<T>` opacos, tipados e sem construtor por bits;
- [x] zero completo reservado como inválido;
- [x] identidade geracional de arena validada antes do lookup do slot;
- [x] `Vacant`, `Occupied` e `Retired`, sem `wrapping_add`;
- [x] erros tipados para invalid, wrong-arena, arena-gone, stale, overflow e
  falha de alocação;
- [x] crescimento usa `try_reserve`, aritmética checked e reserva antes de
  qualquer transição destrutiva;
- [x] `get` empresta, `remove` move e `destroy_arena` destrói payloads vivos
  exatamente uma vez;
- [x] registry é `!Send + !Sync` deliberadamente; concorrência não é herdada
  da trava global do MVP;
- [ ] adapters `ar_gen_*_i64`, backend C e stdlib ainda usam o contrato MVP e
  só migrarão depois de G2/G3 congelarem payload e AMIR. Não contam como Gold.

#### Registro do payload Gold, pré-requisito do G2 AMIR

- [x] `PayloadLayout` do runtime valida tamanho, alinhamento e limite `isize`;
- [x] `PayloadDescriptor` associa layout e drop glue sem conhecer tipos do
  compilador;
- [x] `OwnedPayload` suporta ZST, alinhamento alto, enum, string e transferência
  por movimento a partir da ABI;
- [x] storage type-erased executa drop exatamente uma vez em `remove` e destroy;
- [x] ponte em `arandu_middle::GenPayloadLayout` deriva tamanho/alinhamento do
  `DataLayout` do alvo, incluindo diferença x86_64/i686 para `str`;
- [x] todo bloco `unsafe` possui invariante local e regressões para movimento,
  alinhamento e double-drop;
- [ ] Miri não está disponível no toolchain Windows 1.97.1 instalado; executar
  no gate Linux/nightly quando o componente estiver disponível;
- [ ] geração e passagem do drop glue pelos backends dependem do contrato AMIR
  tipado da próxima etapa.

### G2 — Contrato AMIR e validação

- explicitar `T` e o domínio nas operações Gen;
- atualizar visitors, pretty, stable hash, validator, DCE, liveness, move
  checker, CFG simplification e monomorfização;
- validar dominância, tipos, arena e unicidade do drop;
- preservar spans do escape e da dereferência.

**Saída:** AMIR inválido é rejeitado antes do backend.

#### Registro de execução do contrato AMIR

- [x] `GenInsert`, `GenGet` e `GenRemove` carregam `payload_ty`, domínio lógico
  da arena e span de origem; backends não precisam recuperar `T` da ABI;
- [x] o domínio compiler-managed é distinto da `GenArena<T>` explícita;
- [x] visitors, pretty-printer e resolução SSA preservam os novos campos e
  operandos;
- [x] o validator rejeita handle, payload e resultado com tipos incompatíveis
  usando ICE reportável antes do backend;
- [x] o stable hash inclui metadados Gen, impedindo early-cutoff entre layouts,
  drop glues ou locais de trap diferentes;
- [x] DCE preserva todas as operações Gen porque alocação, trap, invalidação e
  drop são efeitos observáveis;
- [ ] unicidade de `GenRemove` e drop glue no CFG depende da promoção completa
  da próxima etapa;
- [ ] os adapters C/Cranelift ainda consomem apenas o subconjunto `i64` MVP; a
  migração ABI permanece atômica para G4.

### G3 — Promoção completa no pipeline

- substituir pattern rewrite local por transformação guiada pela análise de
  escape;
- cobrir aliases, block params, branches, loops, projeções e múltiplos usos;
- impedir promoção parcial sob `@NoFallback`/flag global;
- inserir `GenRemove` no ponto correto;
- preservar O010 para retorno de referência ao frame.

**Saída:** código de superfície produz AMIR Gold sem query monolítica ou clone
profundo.

### G4 — Runtime compartilhado e paridade

- remover a tabela C fixa;
- fazer Cranelift e C consumirem o mesmo contrato de runtime/ABI;
- suportar payload genérico, layout do alvo e drop glue;
- padronizar traps e códigos de saída;
- testar fora do checkout com artefatos reais de 32 e 64 bits.

**Saída:** nenhuma regra existe só no host JIT ou no emissor C.

### G5 — Diagnóstico, LSP e observabilidade

- enriquecer O004 com origem, caminho do escape e alternativas stack-first;
- quick fix estruturado para `@NoFallback` quando aplicável;
- relatório opt-in por função/módulo, fora de queries Salsa;
- métricas opt-in de promoções, checks, slots e aposentadorias.

**Saída:** fallback sempre inspecionável, nunca alocação invisível.

### G6 — Endurance, fuzzing e Gold

- fuzz state-machine contra um modelo de referência;
- milhões de ciclos com contador reduzido em build de teste para provar a
  aposentadoria sem ABA;
- differential tests entre stdlib, Cranelift e C;
- sanitizers no runtime C e Miri nos componentes Rust compatíveis;
- stress concorrente apenas para a superfície permitida;
- benchmarks e limites documentados.

**Saída:** RFC Gold e roadmap consolidado.

## 7. Matriz mínima de testes

| Família | Casos obrigatórios |
| --- | --- |
| identidade | chave válida, arena errada, zero e índice inválido |
| fabricação | bits aleatórios, force-generation e handle de outro processo |
| temporal | get após remove, double remove, recycle, overflow, aposentadoria |
| payload | inteiros, ZST, alinhado, struct, enum, string, drop e genérico |
| projeção | field/subobjeto válido, owner stale, bounds e layout diferente |
| arena | destroy com handles vivos, endereço reutilizado e tombstone reclamado |
| ownership | move para fora, destroy, exatamente um drop e leak audit |
| CFG | branch, loop, alias, block param, projeção e múltiplos escapes |
| política | O004 note, `@NoFallback`, CLI strict e O010 hard error |
| backend | AMIR, resultado e trap equivalentes em Cranelift e C |
| alvo | i686, x86_64 e aarch64; size/align e overflow de offsets |
| robustez | OOM, capacity error, fuzz, repetição e estado host envenenado |
| incremental | edição de corpo preserva cutoff de importadores |

Testes de trap rodam em subprocesso; nunca abortam o próprio test runner.

## 8. Critérios de promoção a Gold

- [ ] sem limite artificial de 256 slots ou payload `i64`;
- [ ] nenhum wrap pode revalidar chave stale;
- [ ] identidade de arena é verificável;
- [ ] destruir/recriar arena não revalida handles nem vaza metadados sem limite;
- [ ] handle inválido é reservado e testado;
- [ ] payload genérico respeita `DataLayout` e drop único;
- [ ] AMIR impede lowering ambíguo;
- [ ] pipeline de superfície cobre promoção, get, remove e falhas;
- [ ] Cranelift e C passam a mesma suíte diferencial;
- [ ] targets 32/64 usam artefatos nativos e layouts esperados;
- [ ] O004/O010 são determinísticos e preservados no LSP;
- [ ] fuzz/endurance não encontra ABA, UAF, double-drop ou colisão de arena;
- [ ] benchmark prova custo zero de GenRef quando não há escape;
- [ ] documentação separa garantia segura, unsafe e FFI.
- [ ] GenRef de runtime não é persistente/serializável por acidente.

Até lá, o roadmap informa **MVP concluído / Gold parcial**, não “GenRef
completo”.

## 9. Referências e lições

- [Vale — Generational References](https://vale.dev/vision/safety-generational-references):
  modelo híbrido em que análise estática evita checks. O Arandu não remove
  checks sem prova no CFG.
- [Vale — Memory Safety Strategy](https://verdagon.dev/blog/generational-references):
  combina ownership linear, referências geracionais e regiões. Aqui GenRef
  permanece fallback visível, não substituto do OSSA.
- [Reducing Vale's Memory Management Overhead Through Static Analysis](https://digitalcommons.calpoly.edu/theses/2348/):
  motiva medir e só então eliminar checks por prova.
- [Rust `TryReserveError`](https://doc.rust-lang.org/std/collections/struct.TryReserveError.html):
  referência para separar falha de alocação e overflow de capacidade.
- [LLVM AddressSanitizer](https://clang.llvm.org/docs/AddressSanitizer.html):
  base da campanha C contra use-after-free, double-free e out-of-bounds.
- [Rust `slotmap`](https://docs.rs/slotmap/latest/slotmap/): documenta wrap de
  versão e a armadilha de usar uma chave no mapa errado.
- [`generational-arena`](https://docs.rs/generational-arena/latest/generational_arena/):
  modelo seguro e falível para ABA comum; seu repositório está arquivado.
- [Thunderdome `Arena`](https://docs.rs/thunderdome/latest/thunderdome/struct.Arena.html):
  documenta ressurreição possível via `insert_at` e APIs que ignoram geração.
- [Unity Entities — version numbers](https://docs.unity.cn/Packages/com.unity.entities%401.0/manual/systems-version-numbers.html):
  documenta wrap de versões de 32 bits e identidade baseada em índice+versão.
- [EnTT — robust handling of overflow](https://github.com/skypjack/entt/discussions/875):
  discussão concreta sobre stale IDs em aplicações long-lived.
- [Swift ARC](https://docs.swift.org/swift-book/documentation/the-swift-programming-language/automaticreferencecounting/)
  e [análise de heap da Apple](https://developer.apple.com/videos/play/wwdc2024/10173/):
  expõem custos e semântica de weak/unowned, incluindo trap e retenção de
  metadados.
- [CHERI](https://www.cl.cam.ac.uk/techreports/UCAM-CL-TR-941.html): mostra que
  capability/spatial safety e temporal revocation são garantias diferentes.
- [Fil-C runtime](https://fil-c.org/runtime): evidencia que uma alegação de
  segurança completa também depende da fronteira e de todo o runtime; sua
  escolha por GC não é adotada pelo Arandu.

São insumos, não autoridades sobre a semântica do Arandu. As decisões finais
são as invariantes acima, provadas pela suíte multibackend e multialvo.

## 10. Fora desta campanha

- transformar toda referência em GenRef;
- introduzir GC ou reference counting oculto — ambos são alternativas
  rejeitadas, não trabalhos planejados;
- liberar FFI antes de congelar a ABI;
- compactar handles sem benchmark e prova;
- adicionar lifetimes à superfície;
- mover ownership para parser/type checker ou alterar fronteiras Salsa;
- declarar concorrência segura antes do contrato de tipos correspondente.
