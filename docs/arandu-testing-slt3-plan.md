# SL_T.3 — `std.testing` e diagnósticos de expectativa

**Estado:** concluído no contrato v0.1
**Pré-requisito:** SL_T.2 Gold (runner, protocolo e reporters estáveis)  
**Objetivo:** oferecer uma API de testes expressiva, determinística e segura,
com falhas que preservem expressão, valores e spans sem depender de parsing de
texto humano.

## Decisões de projeto

1. **Expectativas retornam falha estruturada.** `expect` e a família
   `expectEqual{Int,Float,Bool,Str}` não
   usam `panic` como mecanismo de controle. A execução produz um `TestFailure`
   com operação, esperado, encontrado, tipos, spans e mensagem opcional.
2. **Avaliação única.** Cada argumento de expectativa é avaliado exatamente uma
   vez e o valor capturado é formatado depois. Isso evita efeitos duplicados e
   divergências entre a comparação e a mensagem.
3. **IDs e spans são canônicos.** A identidade do caso continua sendo a do
   registry SL_T.2; spans são offsets UTF-8 da fonte original e nunca caminhos
   absolutos ou `Debug` de IR.
4. **Skip explícito.** `skip(reason)` encerra o caso com status `skipped`,
   preservando a razão estruturada. Skip por filtro continua distinto de skip
   decidido pelo programa.
5. **Cleanup determinístico.** Código Arandu usa o `defer` da própria linguagem,
   evitando uma segunda ABI de callbacks no módulo de testes. O runtime mantém
   uma pilha LIFO interna para recursos do harness; falhas secundárias não
   apagam a falha primária.
6. **Temporários contidos.** `temp_dir()` cria um diretório exclusivo abaixo da
   raiz da execução; remoção verifica containment, rejeita symlink/junction de
   escape e é idempotente.
7. **Sem dependência do LSP.** A API vive na stdlib/runtime e no pipeline normal;
   o LSP apenas consome o diagnóstico já estruturado.

## Influências e limites

- Rust demonstra o valor de expectativas precisas e de separar testes ignorados
  de testes executados; `should_panic` também mostra por que casar apenas texto
  de panic é impreciso.
- Go oferece um bom modelo para `Helper`, `Log`, `Skip` e cleanup associado ao
  contexto do teste.
- Zig oferece `expectEqual`, skip explícito e detecção de leaks por allocator de
  teste.
- Não adotaremos fixtures implícitas globais, panic textual como protocolo,
  comparação baseada em `Debug`, nem allocator global obrigatório. Essas
  escolhas evitam ordem escondida, mensagens frágeis e interferência entre
  casos paralelos.

## Fases de implementação

### SL_T.3A — Contrato e lowering — concluído

- Adicionar operações internas para `expect`, comparações tipadas, `fail`, `skip` e
  `log`.
- Definir `TestFailure` no crate de contratos, com operação, esperado,
  encontrado, tipo, span e causa opcional.
- Lowering deve capturar argumentos uma vez e emitir o evento terminal uma vez.
- Cobrir `void`, `Result<void, E>` e os tipos comparáveis do contrato v0.1.
  Valores compostos usam `expect(expressão_booleana, mensagem)` até existir uma
  interface de igualdade estável; não há dispatch especial escondido no runner.

**Gate:** testes de lowering provam avaliação única, spans reais e ordenação
determinística.

### SL_T.3B — Contexto, logs e cleanup — concluído

- Criar contexto por caso com ID, seed, logger limitado e pilha LIFO.
- Definir limite de bytes e quantidade de logs; excesso vira truncamento
  explícito, nunca alocação ilimitada.
- Executar cleanup em sucesso, falha, skip, timeout e crash recuperável.
- Testar cleanup que falha junto com uma expectativa primária.

**Gate:** nenhum recurso de um caso aparece em outro, inclusive com `jobs > 1`.

### SL_T.3C — Temporários seguros — concluído

- Implementar `temp_dir()` com nonce criptograficamente forte ou fonte do
  runtime aprovada, sem usar o nome do teste como único segredo.
- Validar containment com caminhos canônicos e `symlink_metadata` antes de
  remover.
- Ter variantes nativas para Windows e Unix e testes de junction/symlink,
  traversal, colisão e diretório já existente.

**Gate:** um caso nunca remove ou escreve fora da raiz temporária da execução.

### SL_T.3D — Diagnósticos e integração — concluído no protocolo CLI/JSON

- Manter falhas de expectativa como eventos de execução, não `DiagCode` de
  compilação. Elas preservam operação, valores, tipo e span para consumidores.
- Preservar labels, notes, hints e replacements no DTO do runner.
- Reporter humano mostra uma mensagem curta; JSON conserva todos os campos.
- CodeLens e apresentação no editor permanecem em SL_T.5; o LSP não interpreta
  texto humano nem ganha uma segunda execução do harness nesta fase.

**Gate:** humano e JSON consomem o mesmo evento estruturado; a futura integração
do editor deverá consumir esse contrato sem reexecutar ou reinterpretar falhas.

### SL_T.3E — Campanha adversarial e Gold — concluído localmente; matriz no CI

- Testar Unicode/UTF-16, CRLF/LF, valores grandes, bytes não UTF-8, logs no
  limite, múltiplas falhas, skip após cleanup, cleanup em ordem inversa,
  symlink/junction e paralelismo.
- Testar que expectativas não são eliminadas pelo DCE e que não alteram o
  resultado do programa fora do modo de teste.
- Adicionar matriz nativa Windows/Linux/macOS e atualizar documentação de CLI.

**Gate final:** falhas são reproduzíveis, estruturadas e completas; não há
P0/P1 aberto no escopo; testes passam com `jobs 1` e `jobs N`.

## Sequência de validação

Cada subfase deve executar os comandos obrigatórios do `AGENTS.md`, além dos
testes focados de `arandu_codegen`, `arandu_cli`, `arandu_runtime` e do contrato
JSON. Snapshots não devem conter duração absoluta, caminhos temporários ou
ordem de mapas.

## Fora do SL_T.3

- benchmarks (`SL_T.4`);
- exportação JUnit e comparação de baseline (`SL_T.5`);
- CodeLens e integração completa com VS Code (`SL_T.5`);
- fuzzing genérico e coverage automática.
