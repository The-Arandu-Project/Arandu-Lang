# Arandu — Salsa, LSP e Identidades (v0.1)

**Status:** caminho arquitetural implementado (F0–F5, inclusive delta por item/bloco); maturidade de produto acompanhada no [roadmap gold do LSP/editor](./arandu-lsp-editor-gold-roadmap-v0.1.md).
**Plano de produto:** [`arandu-lsp-editor-gold-roadmap-v0.1.md`](./arandu-lsp-editor-gold-roadmap-v0.1.md).
**Dono do grafo de queries:** `arandu_query` apenas.

## Salsa toca / não toca

| Crate | Papel | Salsa? |
|-------|--------|--------|
| `arandu_query` | `ArandCompilerDb`, `DatabaseImpl`, `SourceFile`, `AnalysisHost`, `#[salsa::tracked]` | **Dono** |
| `arandu_middle` | `SourceDatabase` trait, tipos, AMIR/HIR, IDs densos | Interface + dados |
| `arandu_resolve` / `arandu_typeck` / `arandu_mir` | Lógica pura; fronteira só | Fronteira só |
| `arandu_lexer` / `arandu_parser` / `arandu_base` / backends | Puros | **Nunca** |
| `arandu_cli` / `arandu_lsp` | Orquestram DB + edits | Cliente do grafo |

### Queries tracked

| Query | Estado |
|-------|--------|
| `parse`, `resolve`, `module_signatures`, `type_check`, `lower_amir` | Reais |
| `local_symbols`, `exported_symbols`, `func_amir` | Reais |
| `liveness_facts` | Real (`arandu_mir::liveness`) |
| `block_dataflow_facts` | live/init/moved/stmt counts por bloco |
| `func_analysis_diags` / `block_diagnostics` / `file_ide_diagnostics` | F4 — diags IDE memoizados |
| DX.5 `RebuildLog` | Opt-in (`-Zexplain-rebuild`) |

### I/O de fonte

- typeck/resolve: proibido `fs::read` (guardrail `architecture_invariants`).
- Registro: CLI / LSP / `DatabaseImpl::resolve_module_path` (fallback disco **só** na DB).
- Workers LSP **não** registram arquivos; só a main.

## Três identidades

| ID | Geracional? | Função |
|----|-------------|--------|
| `DocumentId` (`slotmap`) | **Sim** | Buffer LSP; close → stale |
| `FileId` + densos | **Não** | Análise na revisão atual |
| `AnalysisRevision` | Sim (host) | Handles IDE não atravessam edit |

`LspSymbolId { symbol, revision }` — resolve só se `revision == snap.revision`.

**Deadlock Salsa:** nunca segurar `AnalysisSnapshot` / clone de `DatabaseImpl` na **mesma** thread que chama `set_text` (Storage espera clones == 1).

## Legado

| Item | Status |
|------|--------|
| `CompileSession` | **Removido** |
| `symbol_span` dummy | **Span real** + `try_get` safe |
| tower-lsp / tokio no path de query | **Removidos** do `arandu_lsp` |

## LSP gold (implementado)

1. Main síncrona (`lsp-server`) + `Vfs` debounce 100 ms.  
2. Workers: `AnalysisSnapshot` (clone Storage) → diags/goto; publish só se DocumentId vivo e revision match.  
3. didChange **não** commita Salsa por tecla; flush no debounce / didSave / goto.  
4. Diagnostics via `file_ide_diagnostics` (F4); fingerprint blake3 evita republish no-op.  
5. CST-first Rowan: `syntax_tree` tenta reparse do ITEM tocado e reutiliza os green nodes irmãos; fallback seguro faz parse completo.
6. `initialize` conclui antes de I/O do workspace; a descoberta determinística e
   limitada ocorre em worker, e cada fonte retorna à main para registro na DB.
7. O scheduler mantém no máximo 64 jobs pendentes, serve a fila interativa
   antes da fila ampla, coalesce diagnósticos por `DocumentId` e cancela
   requests obsoletos antes de uma revisão nova. `$/cancelRequest` responde
   com `RequestCancelled`, inclusive quando o job ainda não começou.
8. O servidor negocia UTF-16 explicitamente e todas as conversões entre bytes
   UTF-8 e posições LSP passam pelo mesmo `LineIndex`; semantic tokens usam
   comprimentos UTF-16 e são divididos por linha.
9. Edições recebidas dentro do debounce compõem sobre o buffer pendente da VFS,
   inclusive múltiplas mudanças por notificação, Unicode, arquivo vazio e EOF.
10. `IdeDiagnostic` preserva labels, notes, hints e replacements nas queries;
    o wire publica versão, `codeDescription`, `relatedInformation`, tags e
    `Diagnostic.data`. Quick fixes consomem apenas replacements estruturados.
11. Hover, completion e signature help compartilham apresentação de assinatura,
    tipos e doc comments; nenhum DTO expõe `Debug` de IR ou `SymbolId`.
12. Fontes conhecidas do workspace e overlays abertos têm autoridades distintas:
    overlay vence enquanto aberto, `didClose` restaura o disco e invalida o
    `DocumentId`, e create/delete/rename usam filtros `**/*.aru`. URI Windows
    padrão e caminho verbatim convergem para uma identidade; `FileId` removido
    nunca é reutilizado.
13. Depois do handshake, a descoberta em background instala manifesto,
    `ModuleRoots`, stdlib e `DirectoryListing` na thread escritora. Mudanças
    estruturais atualizam uma única listagem Salsa e reanalisam importadores
    abertos; `resolve` declara essa listagem como dependência explícita, enquanto
    edições somente de corpo preservam o cutoff de exports. Chaves absoluta,
    qualificada e relativa podem apontar ao mesmo `SourceFile`, sem perder o
    índice reverso enquanto algum alias continuar vivo.

## F4 / P3 — delta on-type

- `block_dataflow_facts`: live/init/moved/stmt por bloco.  
- **`item_ide_diagnostics`**: diags de typeck **por item** (`item_body_typeck`) + AMIR se func.  
- **`file_ide_diagnostics`**: union barata dos memos de item + signatures.  
- Early cutoff entre itens (testes `item_body_cutoff`, `ide_diag_delta`).  
- Typeck monólito substituído por compose P1/P2; wire LSP ainda manda lista full (protocolo).

## P5 — CST-first (rowan)

- **Canônico:** `syntax_tree(file)` a partir do texto (ITEM por heurística de keywords).  
- **`parse(file)`** = `lower_syntax_to_program(syntax_tree)` — AST só como lower do CST.  
- **`reparse_subtree`**: re-lex só o ITEM tocado + `replace_child` (green dos irmãos reutilizado); fallback full `parse_syntax`.  
- **`syntax_tree` Salsa**: cache por file + `single_contiguous_edit` → `reparse_subtree`.  
- **Lower sem re-lex**: tokens no `SyntaxTree`; `parse_token_stream`.  
- **LSP semantic tokens** via query `file_highlights` (CST + resolve → `HlKind`; `textDocument/semanticTokens/full`).  
- Fingerprint de item (`item_source_input`) usa texto do ITEM CST.  
- Typeck/resolve consomem AST **somente** via lower do CST (`parse` ← `syntax_tree`).

## Guardrails / testes

- `architecture_invariants`, `doc_store` stale, `analysis` revision stale, `vfs` debounce, `block_delta`.
