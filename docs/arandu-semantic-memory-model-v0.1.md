# Arandu Semantic Memory Model v0.1

**Status:** arquitetura aceita, parcialmente implementada. O subconjunto local,
o fallback GenRef e referências relativas de coroutine existem; borrowed views
estruturais permanecem em campanha até a promoção de `SL_S-Core`.

## Visão Geral e Contexto

O Arandu trata memória como consequência da semântica demonstrada no fluxo do
programa. O modelo não é apenas um borrow checker e não pretende substituir uma
análise precisa de loans por checks dinâmicos. Ele conecta, na mesma IR,
ownership, liveness, escape, representação física e diagnóstico:

```text
OSSA demonstra janela local
          │
          ├─ sim ──→ borrow direto; nenhum custo de runtime
          │
          └─ não
             ├─ referência interna de coroutine representável
             │      └─→ RelativeBorrow; endereço resolvido no frame atual
             ├─ owner escapável representável
             │      └─→ GenRef + O004; fallback explícito e observável
             └─ relação sem representação segura
                    └─→ diagnóstico fail-closed antes do backend
```

O objetivo não é declarar o Arandu “melhor que Polonius”. Polonius é uma
família de análises de origins, loans, subsets e pontos do CFG. O diferencial
pretendido pelo Arandu é arquitetural: reutilizar fatos OSSA e liveness em toda
a cadeia e definir o que ocorre quando a prova estática termina, sem RC/ARC,
tracing GC ou promoção silenciosa como resposta universal.

A sintaxe pública permanece `T`/`own T`, `ref T` e `mut ref T`. Lifetimes,
endereços crus e detalhes da estratégia de fallback não aparecem na superfície
segura apenas para satisfazer a implementação do compilador.

## Detalhes Técnicos da Implementação

### Contratos já implementados

O AMIR representa `move`, `copy`, `Borrow`, `BorrowMut`, `Destroy`,
`StorageLive` e `StorageDead`. `arandu_mir::borrow_facts` associa cada loan ao
owner raiz e aos temps/locals que carregam a referência. A janela do loan é a
live range desses holders, calculada pela mesma infraestrutura de liveness
usada pelo middle-end; não existe um segundo motor lexical de lifetimes.

BV.1 substituiu o resumo escalar pelo contrato estrutural
`ReturnBorrowSummary { dependencies[] }`. Cada dependência contém caminho do
resultado, conjunto canônico de fontes formais (índice + caminho no argumento)
e `BorrowKind`. O contrato é demonstrado pelo fluxo AMIR, aceita união de
múltiplas origens realmente alcançáveis, compõe calls/imports/genéricos e é
resolvido por worklist até o menor fixpoint em recursão. A call AMIR carrega o
resumo convergido antes de borrow/escape checking; os backends nunca observam
metadata incompleta.

`TypeInfo::borrow_paths` inspeciona `Option`, `Result`, tuple, struct, enum e
outros carriers por caminhos estáveis, interrompe tipos nominais recursivos e
falha fechado acima de 32 segmentos ou 256 folhas. A query Salsa estreita
`borrow_interfaces` publica apenas contratos ordenados: uma edição corporal
recalcula a prova local, mas o hash igual impede a invalidação do caller.

Borrows absolutos de locals que atravessariam uma suspensão são rejeitados por
O010. Quando a referência aponta para um slot elegível do próprio frame, A3.4
a reescreve como `RelativeBorrow { local, mutable }`: o valor materializado é
uma identidade relativa, e cada load consulta o home atual do frame. Isso evita
um endereço interno obsoleto caso a coroutine mude de storage.

Escape analysis classifica referências cuja janela deixa o CFG local. Retornar
um borrow de local continua erro O010. Um owner raiz cuja promoção seja
representável pode usar GenRef e sempre produz O004; `@NoFallback` e
`--no-generational-fallback` convertem essa saída em erro. GenRef valida
identidade geracional e lifetime do slot, mas não prova aliasing, projeções,
thread safety, persistência ou FFI.

Virtual anchoring preserva a identidade semântica dos locals enquanto as
análises OSSA executam. Os anchors são removidos antes do backend, depois que
move checking, borrow checking e drop elaboration consumiram os fatos.

### Modelo normativo de prova

Cada valor de referência seguro possui:

- um `BorrowKind`: compartilhado ou exclusivo;
- um conjunto de owners formais ou locais;
- zero ou mais caminhos estruturais do resultado que carregam a dependência;
- holders em locals, temps, block arguments ou carriers;
- uma janela definida pela liveness dos holders no CFG.

As regras fundamentais são:

1. uma origem nunca é fabricada a partir de endereço cru;
2. todos os caminhos de controle que alcançam um uso participam do join;
3. vários leitores podem coexistir; um acesso exclusivo conflita com qualquer
   outro holder vivo do mesmo owner;
4. move, destroy ou mutação do storage conflitam com uma view ainda viva;
5. `mut ref` não pode ser duplicado, inclusive dentro de aggregate;
6. um carrier herda não-escapabilidade e exclusividade dos valores contidos;
7. qualquer relação não modelada falha antes de C ou Cranelift;
8. metadata de borrow é apagada após a prova e não altera o ABI do valor.

### Resumo interprocedural demonstrado pelo fluxo

A assinatura define quais relações são legais, mas não deve inventar quais
parâmetros o corpo realmente retorna. O contrato Gold é calculado a partir do
fluxo tipado e publicado como um resumo canônico:

```text
BorrowInterface
└── return_dependencies[]
    ├── result_path
    ├── formal_origins[]
    └── kind
```

Para `func primeiro(a: ref T, b: ref T): ref T { return a }`, o resumo contém
somente `a`. Se branches retornarem `a` e `b`, o join contém ambos. A união é
conservadora apenas sobre origens que realmente alcançam aquele caminho de
retorno; compatibilidade de tipo sozinha não basta.

A análise implementada é uma transferência simbólica pura sobre o CFG/OSSA. Formais começam
como origens simbólicas; borrow de local é marcado como origem local; use,
load, store, aggregate, projection e block argument propagam conjuntos. Calls
aplicam o resumo do callee. Recursão é resolvida deterministicamente por
worklist até fixpoint no domínio finito; limite excedido, ausência de origem ou
relação irrepresentável falha fechado.

O solver pertence à lógica pura de `arandu_mir`. `arandu_query` continua sendo
o único dono da execução Salsa, da memoização e da composição incremental. A
query pública expõe somente o resumo ordenado e hash-estável, sem spans,
`TypeId` internado ou ordem de `HashMap`.

### Early-cutoff por contrato

O corpo é uma entrada para demonstrar a interface, mas callers não dependem do
corpo inteiro:

```text
body edit
   ↓
recalcula BorrowInterface do item
   ├─ resumo igual  → HashEq/early-cutoff; callers permanecem verdes
   └─ resumo mudou → invalida apenas callers que consomem o contrato
```

Esse desenho preserva a separação entre corpo e superfície exportada. Uma
alteração interna que não muda dependências de retorno não causa recompilação
transitiva; uma alteração que muda a proveniência pública deve invalidar os
consumidores por segurança.

### Hierarquia de representação

O compilador escolhe a primeira representação comprovadamente suficiente:

| Situação | Representação | Custo/garantia |
| --- | --- | --- |
| loan limitado pelo CFG | borrow direto | zero metadata/check de runtime |
| borrow interno elegível atravessa `await` | `RelativeBorrow` | índice relativo; sem endereço self-referencial obsoleto |
| owner raiz representável escapa | GenRef | validação geracional; O004 observável |
| projeção/view sem owner-path seguro | nenhuma | rejeição fail-closed |
| operação explicitamente raw | ponteiro em `unsafe` | invariantes ficam com o autor do bloco unsafe |

GenRef não é aplicado a `Slice` ou `ref campo` apenas para fazer o programa
compilar. Uma promoção de view exige uma representação first-class de owner e
caminho que preserve drop, mutação e layout; enquanto isso não existir, o caso
continua rejeitado.

### Borrowed views e stdlib

`Slice<T>` pode conservar o ABI `ptr + len`, mas uma instância segura precisa
carregar uma dependência compile-time do owner. `Option<ref T>`,
`Result<ref T, E>`, tuples, structs e enums propagam a relação transitivamente.
Uma view compartilhada de `Vec` ou `String` impede qualquer operação que exija
`mut ref` do mesmo owner, incluindo operações que possam realocar storage; não
há lista especial de nomes como `push` ou `reserve`.

Construção a partir de raw pointer permanece `unsafe`. Converter um ponteiro
para uma forma de view não recupera proveniência, inicialização ou lifetime que
o compilador não conseguiu demonstrar.

### Relação com trabalhos existentes

- Polonius fornece o vocabulário preciso de origins, loans, subsets, liveness e
  invalidation sobre CFG. Arandu reutiliza esses princípios, mas não adota
  obrigatoriamente a implementação Datalog nem mantém origins como uma segunda
  realidade separada do OSSA.
- NLL demonstra que a duração útil de um borrow deve seguir o uso no CFG, não
  somente o bloco lexical.
- Swift `~Escapable` e `Span` mostram que não-escapabilidade precisa propagar
  por containers e que uma view deve bloquear mutação incompatível do owner.
- Vale mostra o uso de referências geracionais para relações que uma disciplina
  puramente estática não expressa. Arandu limita isso a fallback classificado,
  visível e recusável.

Referências primárias:

- [Polonius — loan analysis](https://rust-lang.github.io/polonius/rules/loans.html)
- [Rust RFC 2094 — non-lexical lifetimes](https://rust-lang.github.io/rfcs/2094-nll.html)
- [Swift SE-0446 — nonescapable types](https://github.com/swiftlang/swift-evolution/blob/main/proposals/0446-non-escapable.md)
- [Swift SE-0456 — `Span`-providing properties](https://github.com/swiftlang/swift-evolution/blob/main/proposals/0456-stdlib-span-properties.md)
- [Swift SE-0465 — nonescapable stdlib primitives](https://github.com/swiftlang/swift-evolution/blob/main/proposals/0465-nonescapable-stdlib-primitives.md)
- [Vale — generational references](https://vale.dev/memory-safe)

## PONTOS DE MELHORIA (O que não está no roadmap)

Não há ainda modelo formal publicado que prove soundness da composição entre
OSSA, `RelativeBorrow` e GenRef. A implementação também não mede sua precisão
ou custo contra Polonius; portanto “mais rápido”, “mais preciso” ou “melhor”
permanecem hipóteses, não propriedades do produto.

O fallback atual cobre owners raiz representáveis, não projeções arbitrárias,
concorrência, persistência ou FFI. Uma futura extensão owner/path exigirá RFC,
modelo executável, Miri/sanitizers e corpus adversarial próprios.

## Futuro e Próximos Passos

A campanha Borrowed Views Gold implementará `BorrowInterface` estrutural,
solver por fluxo/SCC, carriers não escapáveis e APIs seguras de `Slice` e
`String`. Depois da campanha, o modelo deve ganhar um microcálculo formal e um
oráculo executável que compare resultados do solver com casos reduzidos.

Comparações de desempenho ou precisão com Polonius devem usar corpus comum,
classificação prévia de casos, tempo/memória de compilação e conjuntos de
programas aceitos/rejeitados. Nenhuma decisão de estrutura de dados será tomada
apenas por contagem de linhas, alocações ou alegação teórica sem benchmark.
