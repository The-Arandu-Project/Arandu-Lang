# Arandu — Roadmap Gold do LSP e Editor

**Status:** ativo em paralelo ao roadmap de estabilização.  
**Objetivo:** usar o editor como instrumento de hardening até oferecer uma
experiência beta gold previsível no VS Code.  
**Arquitetura normativa:** [`arandu-salsa-lsp-architecture-v0.1.md`](./arandu-salsa-lsp-architecture-v0.1.md).

## Decisão de arquitetura Gold

O alvo não é copiar a quantidade de features do rust-analyzer nem executar o
workspace inteiro a cada tecla. O Arandu combina:

- handshake e arquivo aberto primeiro, com descoberta progressiva do workspace;
- VFS como única visão de arquivos e Salsa sem I/O nas queries;
- snapshots canceláveis, fila limitada e prioridade para requests interativos;
- diagnóstico rápido do arquivo aberto e análise ampla somente após idle/save;
- operação parcial durante indexação, reload e código quebrado;
- métricas reproduzíveis de cold start, latência, memória, backlog e stale work.

Auditoria do código em 2026-08-21 encontrou dois bloqueadores imediatos. O
primeiro foi removido em L0-A; o segundo pertence a L1-A:

1. ~~`initialize` chama `walk_register_aru` e visita até 256 arquivos antes de
   responder ao cliente~~ — resolvido: o handshake agora precede toda descoberta;
2. revisões antigas são descartadas com segurança, mas os jobs continuam
   consumindo CPU em uma fila sem backpressure nem cancelamento LSP.

### Referências e problemas que não vamos herdar

- [gopls diagnostics](https://go.dev/gopls/features/diagnostics): feedback do
  arquivo aberto em dezenas de milissegundos e análise ampla após idle; não
  adotaremos diagnóstico de todo o workspace a cada tecla.
- [gopls workspace](https://go.dev/gopls/workspace): escopo orientado aos
  arquivos abertos reduz configuração, mas workspaces amplos podem elevar
  startup e memória; Arandu terá descoberta progressiva e limites explícitos.
- [gopls file watching](https://github.com/golang/tools/blob/master/gopls/internal/settings/settings.go):
  clientes e sistemas de arquivos perdem ou duplicam eventos de diretório e
  padrões amplos têm custo alto; overlays abertos vencem eventos do disco e os
  filtros do Arandu ficam restritos a `**/*.aru`.
- [Watchman case-insensitivity](https://facebook.github.io/watchman/docs/casefolding):
  caixa, caminhos verbatim e renames podem representar um arquivo de formas
  diferentes; a fronteira URI/caminho do Arandu normaliza essas identidades e
  nunca reutiliza `FileId` depois de delete/rename.
- [gopls scalability](https://go.dev/blog/gopls-scalability): resumos e índices
  persistentes reduzem recomputação; cache em disco só entra depois de formato,
  invalidação, versão e corrupção terem contrato e testes próprios.
- [rust-analyzer architecture](https://rust-analyzer.github.io/book/contributing/architecture.html):
  VFS, dados derivados lazy, isolamento de I/O e cancelamento são referências;
  não adotaremos carregamento global bloqueante nem manteremos todo resultado
  pesado vivo sem orçamento/LRU.
- [LSP 3.18](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.18/specification/):
  encoding negociado, cancelamento e progresso são contratos de protocolo, não
  detalhes exclusivos da extensão VS Code.
- [VS Code extension testing](https://code.visualstudio.com/api/working-with-extensions/testing-extension):
  a integração será exercitada em Extension Host real.
- [clangd index](https://clangd.llvm.org/design/indexing): índice dinâmico dos
  arquivos abertos cobre a experiência imediata e é mesclado ao índice de
  background; Arandu adotará as camadas, mas não um index global obrigatório.
- [clangd features](https://clangd.llvm.org/features): completion contextual,
  documentação no hover, fixes, outline e tokens padrão definem a barra de
  usabilidade, sem importar custos específicos do frontend C++.
- [Pyright](https://microsoft.github.io/pyright/): CLI, servidor e extensão
  compartilham o mesmo checker; Arandu preserva a mesma fonte semântica entre
  CLI/LSP, sem criar um segundo compilador no TypeScript.
- [VS Code language features](https://code.visualstudio.com/api/language-extensions/programmatic-language-features):
  matriz canônica de recursos do editor e seus métodos LSP.
- [VS Code semantic highlighting](https://code.visualstudio.com/api/language-extensions/semantic-highlight-guide):
  o servidor classifica tokens; temas escolhem cores. Tipos/modificadores
  padrão e scopes TextMate são obrigatórios para compatibilidade entre temas.
- [VS Code UX](https://code.visualstudio.com/api/ux-guidelines/overview): usar
  Problems, Output, Status Bar, progress e notificações nativas; webview apenas
  quando a API nativa não resolver o caso.

### Orçamentos iniciais

Medidos em corpus versionado e runners nativos; p95 é o gate, p50 é informativo.

| Operação | Cold p95 | Warm p95 | Regra |
|---|---:|---:|---|
| resposta a `initialize` após receber request | 250 ms | 100 ms | zero scan/análise do workspace |
| diagnóstico do arquivo aberto após debounce | 750 ms | 250 ms | nunca aguarda análise global |
| completion / hover / goto | 300 ms | 100 ms | prioridade interativa |
| cancelamento de request obsoleto | 100 ms | 50 ms | sem resposta de sucesso stale |

Esses números são hipóteses de produto. L0 cria o harness e registra o baseline;
se um runner provar ruído estrutural, o orçamento muda por decisão documentada,
nunca para esconder regressão.

## Estado auditado

| Capacidade | Estado | Falta para gold | Dono |
|------------|--------|-----------------|------|
| VFS, debounce, snapshots e stale safety | `done` | startup progressivo, cancelamento e stress | LSP/query |
| Diagnósticos on-type | `partial` | preservar labels, notes, hints, replacements, versão e links dos códigos | query/LSP |
| Goto e references | `done` | multi-file/Unicode e operação durante index parcial | LSP |
| Hover | `done` | — | LSP, dados já existem |
| Completion | `partial` | contexto/escopo, ranking, snippets, resolve e índice parcial | LSP |
| Signature help | `done` | — | LSP |
| Rename | `partial` | `prepareRename`, nomes inválidos, conflitos e preview | LSP/extensão |
| Document/workspace symbols | `partial` | hierarquia, container, filtro fuzzy e índice progressivo | LSP/index |
| Semantic tokens | `partial` | corrigir enum/constant, tokens multi-linha, encoding e snapshots de temas | query/LSP/extensão |
| TextMate e edição básica | `partial` | paridade gerada/testada com lexer; word pattern, on-enter e folding | extensão/parser |
| Formatter e quick fixes | `partial` | range/on-type, idempotência E2E e transportar replacements reais | fmt/LSP |
| Logs, erros e status | `partial` | ligar trace, Output estruturado, progress, crash/restart sem spam | extensão/LSP |
| Identidade visual | `missing` | ícone Marketplace, ícone `.aru` light/dark, banner e screenshots | extensão/design |
| Extensão VS Code | `partial` | lint, testes Extension Host, package/instalação e acessibilidade | extensão |
| Debugger e visualizações avançadas | `planned` | fora do beta gold inicial | pós-gold |

## Contrato visual e de UX

### Cores e tokens

- O servidor nunca envia RGB. Ele emite tipos/modificadores semânticos e a
  extensão fornece scopes TextMate de fallback para temas que não entendem
  semantic tokens.
- Usar tipos LSP padrão sempre que houver equivalência: `function`, `method`,
  `variable`, `parameter`, `type`, `typeParameter`, `struct`, `enum`,
  `enumMember`, `interface`, `namespace`, `property`, `number`, `string`,
  `comment`, `operator`, `keyword`.
- Corrigir a inconsistência atual em que a posição 14 da legenda é
  `enumMember`, mas `HlKind::Constant` ocupa esse índice, e em que variantes de
  enum são classificadas como `enum` em vez de `enumMember`.
- Testar TextMate antes da inicialização e semantic tokens depois dela nos
  temas Dark+, Light+ e High Contrast. Um tema Arandu é opcional e nunca é
  ativado automaticamente.

### Ícones e apresentação

- Adicionar ícone de Marketplace, `galleryBanner`, repository, bugs, homepage,
  license e keywords ao manifest.
- Adicionar ícones light/dark da linguagem para arquivos `.aru`. Não publicar
  um file-icon theme completo apenas para substituir ícones de outras linguagens.
- Usar Codicons no status e comandos. O status global fica à esquerda, com
  estados discretos: starting, indexing, ready, degraded e error.
- README da extensão precisa de screenshots reais em tema claro/escuro,
  instalação, descoberta do servidor, settings e troubleshooting.

### Erros, logs e recuperação

- Problems recebe código, severidade, mensagem, span, versão do documento,
  `codeDescription` para `docs/errors/<CODE>.md`, labels como
  `relatedInformation`, notes/hints e tags quando semanticamente corretas.
- Replacements do compilador viram quick fixes específicos; a extensão não
  deve inferir correções analisando strings de mensagens.
- Output `Arandu Language Server` contém versão, caminho do binário, raiz,
  estado de indexação, request id/duração e falhas acionáveis, sem source text,
  tokens ou caminhos sensíveis por padrão. Trace protocol é opt-in.
- Startup/ready não gera toast. Notificações aparecem apenas para falha que
  exige ação e oferecem `Open Logs`, `Configure Server` ou `Restart`.
- Panic isolado não mata a sessão; crash do processo usa política de restart
  limitada para não entrar em loop.

### IntelliSense

- Completion respeita escopo e visibilidade, distingue símbolos homônimos e
  fornece `kind`, assinatura em `detail`, doc comment, `filterText`, `sortText`,
  `textEdit` e snippets apenas quando o cliente suporta.
- Keywords estruturais podem inserir corpo/tabstops; funções podem inserir
  parênteses e parâmetros. Auto-import só entra após edits multi-file e
  resolução de conflitos serem seguros.
- Resposta durante indexação pode ser incompleta, mas nunca incorreta; o cliente
  recebe `isIncomplete` para solicitar novamente.
- Hover mostra assinatura Arandu e documentação, não `Debug` de tipos nem
  `SymbolId`. Signature help e completion compartilham o mesmo formatador de
  assinaturas para não divergir.

## Dependências da linguagem/compilador

O editor Gold não exige novas construções sintáticas. A maior parte do trabalho
é exposição correta de dados existentes. Dependências reais:

| Necessidade IDE | Situação no compilador | Decisão |
|---|---|---|
| documentação em hover/completion | doc comments já são anexados e chegam a `ResolutionResult.docs` | expor por query IDE; sem mudar sintaxe |
| diagnósticos ricos/quick fixes | `Diagnostic` já tem labels, notes, hints e replacements | preservar no `IdeDiagnostic`; sem novo checker |
| completion por escopo/tipo | symbols, resolve e `TypeInfo` existem | criar query/DTO IDE estreito |
| ícones/tipos semânticos | `SymbolKind` existe | corrigir mapeamento e legenda |
| rename seguro | identidade existe; validação de conflito é incompleta | query pura de prepare/validate rename |
| auto-import | módulos/exported symbols existem | adiar até workspace index + edits seguros |
| inlay hints de tipos | tipos resolvidos existem | pós-Gold opcional; não muda linguagem |
| doc links entre símbolos | doc comments existem, links não têm resolução formal | pós-Gold ou RFC própria se necessário |
| folding/selection range | CST Rowan contém estrutura/spans | implementar no LSP, sem AST paralela |
| snippets básicos | gramática da linguagem já define construções | extensão/LSP; não altera parser |

Não criar sintaxe, atributos ou metadados apenas para decorar o VS Code. Uma
mudança de linguagem só será aberta quando o recurso tiver semântica útil fora
do editor e contrato próprio no roadmap mestre.

## L0 — Gate da extensão

- [x] `npm ci` e `npm run compile` reproduzíveis.
- [x] ESLint tipado com zero warnings; placeholder removido do gate.
- [x] Testes automatizados no VS Code Extension Host mínimo suportado.
- [x] Descoberta do `arandu-lsp` testada para PATH, configuração explícita,
      `.exe` no Windows e layouts release/debug.
- [x] Crash, restart e logs apresentam estado acionável ao usuário.
- [x] Manifest completo, identidade visual e package `.vsix` auditado.
- [x] TextMate/semantic tokens testados em Dark+, Light+ e High Contrast.
- [x] Harness stdio executa processo real, mede initialize/diagnóstico/requests
      e aplica gate de 250 ms ao p95 do handshake com workspace adversarial.
- [x] `initialize` responde antes de qualquer caminhada ou análise do workspace.
- [x] Descoberta pós-handshake usa backlog limitado e prioridade inferior a
      arquivos abertos, resultados interativos e requests do cliente.

**DoD L0:** a extensão tem o mesmo nível mínimo de gate que o workspace Rust.

## L1 — Correção de protocolo e texto

- [x] Negociar e testar position encoding suportado; UTF-16 permanece correto para clientes que o exigem.
- [x] Cobrir Unicode antes/depois do cursor em todos os requests semânticos.
- [x] Dividir semantic tokens multi-linha em tokens válidos por linha.
- [x] Testar mudanças incrementais múltiplas, arquivo vazio e edição no fim do arquivo.
- [x] Validar cancelamento/descarte de jobs obsoletos sob rajadas de edição.
- [x] Implementar `$/cancelRequest`, fila limitada, coalescing por documento e
      prioridade interativa sobre index/diagnóstico amplo.
- [x] Preservar diagnósticos ricos e replacements end-to-end.
- [x] Completar hover/completion/signature com docs e apresentação consistente.

**DoD L1:** nenhuma posição, token ou diagnóstico incorreto em ASCII/Unicode no protocolo suportado.

## L2 — Dogfooding multi-file

- [x] Abrir, fechar, criar, excluir e renomear arquivos durante uma sessão.
- [x] Imports locais e stdlib atualizam completion/goto/diagnósticos sem restart.
- [x] Rename detecta nome inválido e conflitos; `prepareRename` fornece o span
      exato, edits multi-file são ordenados/deduplicados e o preview pertence à
      extensão, não à query pura.
- [x] Formatter-on-save é opt-in; o formatter canônico é idempotente e retorna
      edits mínimos por linha ou por hunk para preservar cursor e seleção fora
      da região realmente alterada.
- [ ] Testar vários documentos abertos e requests concorrentes em snapshots.
- [ ] Medir p50/p95 de diagnóstico, completion, goto e rename em corpus versionado.
- [ ] Folding, selection ranges e document highlights baseados no CST/resolve.
- [ ] Validar Problems, Output, status, progress e recuperação de crash no editor.

**DoD L2:** o editor pode ser usado diariamente para desenvolver os projetos do corpus.

## L3 — Beta gold do editor

- [ ] Instalação da extensão e do servidor documentada e testada nos hosts suportados.
- [ ] Matriz de capabilities publicada com limitações conhecidas.
- [ ] Sem crash, deadlock ou publicação stale em campanha de stress definida.
- [ ] Diagnósticos, navigation, rename, tokens e format passam no Extension Host.
- [ ] Release candidate dogfood sem bloqueador conhecido.

**DoD L3:** VS Code + `arandu-lsp` formam uma experiência beta gold dentro da matriz publicada.

## Ordem de implementação

1. **L0-A — baseline e startup (concluído):** harness stdio, métricas,
   `initialize` sem scan e descoberta de baixa prioridade.
2. **L0-B — extensão confiável:** lint, Extension Host, descoberta e restart.
3. **L1-A — scheduler (concluído):** cancelamento, fila limitada, coalescing e prioridades.
4. **L1-B — protocolo/texto:** encoding, Unicode, semantic tokens e edits.
5. **L2 — sessão real:** workspace dinâmico, multi-file, rename e performance.
6. **L3 — promoção:** matriz nativa, stress, dogfood e RC sem bloqueadores.

## Depois do gold

Debugger, valores inline, borrow gutter, Test Explorer, CodeLens de paridade,
timeline de corrotinas e overlay Salsa permanecem propostas pós-gold. Elas não
devem interromper L0–L3, salvo quando uma delas for necessária para reproduzir
ou diagnosticar um bloqueador de estabilidade.

Também ficam pós-gold: inlay hints de tipos/escape, postfix completions,
geração de stubs de interface, safe delete/extract, REPL, documentação ao vivo,
package explorer, bindgen wizard, análise de alocação e auto-import complexo.
