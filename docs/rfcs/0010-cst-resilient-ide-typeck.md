# RFC 0010: Pipeline CST-First Resiliente e Typeck Incremental para IDE

- **Número da RFC:** 0010
- **Título:** Pipeline CST-First com Rowan, Reparse de Sub-árvore e Type-Checking Incremental
- **Autor(es):** Equipe do Compilador Arandu
- **Data de Início:** 2026-07-20
- **Status:** `Implemented` (Crates `arandu_parser`, `arandu_query`, `arandu_lsp`)
- **Área Principal:** `Frontend` / `IDE` / `Incrementalidade`
- **Substitui:** `docs/arandu-typeck-ide-cst-plan-v0.1.md` (resgatada do histórico de commits)

---

## 1. Resumo (Summary)

Esta RFC formaliza a arquitetura do frontend do compilador Arandu baseada em **Árvore de Sintaxe Concreta (CST-first)** utilizando a biblioteca Rowan. O compilador abandona a dependência de um parser tradicional frágil que falha diante do primeiro erro de sintaxe, adotando uma árvore completa que preserva 100% dos caracteres do código-fonte (incluindo espaços em branco e comentários), com suporte a **reparse incremental de sub-árvore (*subtree reparse*)** e recuperação sintática graciosa.

---

## 2. Motivação (Motivation)

Servidores de linguagem (LSP) operam sobre código em edição ativa, o qual passa 90% do tempo sintaticamente incompleto (ex: um bloco `func` aberto sem fechar chaves, ou uma expressão interrompida enquanto o usuário digita).
* Parsers tradicionais em lote abortam ou geram árvores incompletas, desativando recursos como realce de sintaxe (*semantic tokens*), completion e hover no editor.
* Re-executar o lexer e parser do arquivo completo a cada caractere digitado desperdiça ciclos de CPU e degrada a bateria em notebooks.

O Arandu resolve esses desafios com um pipeline em camadas:
1. **CST Rowan (`syntax_tree`)**: Árvore verde imutável e barata de clonar via `Arc`, tolerante a erros e proprietária de todos os tokens.
2. **Reparse Localizado**: Edições contidas dentro de um único item (ex: corpo de uma função) re-analisam apenas aquele item, reaproveitando os nós irmãos intactos.
3. **Lowering Direto para AST (`parse`)**: A AST é derivada diretamente do fluxo de tokens do CST já memorizado, sem re-ler o arquivo do disco ou refazer análise léxica em texto bruto.

---

## 3. Fluxo Canônico do Pipeline

```text
SourceFile.text
    │
    ▼
syntax_tree(file)      // CST Rowan canônico; cache e reparse_subtree em edits contíguos
    │
    ▼
parse(file)            // Lower sintático para AST tipada sem re-lex
    │
    ▼
resolve(file)
    │
    ▼
item_body_typeck(item) // Type-checking incremental refinado por função/item
    │
    ▼
file_ide_diagnostics   // Emissão e agregação de diagnósticos com delta mínimo
```

---

## 4. Reparse de Sub-árvore (*Subtree Reparse*)

Quando uma edição ocorre no buffer do editor:
1. O compilador calcula o intervalo `[start, end]` da alteração e o novo texto substituto.
2. Se a edição estiver contida dentro dos limites de um único `ITEM` (uma função ou struct):
   * O compilador executa a análise léxica **apenas sobre o texto daquele item**.
   * Constrói o novo nó verde (*green node*) do item.
   * Aplica `replace_child` na raiz da árvore.
   * Todos os itens irmãos reutilizam seus nós verdes anteriores via identidade de ponteiro `Arc::ptr_eq`, com custo de alocação próximo de zero.
3. Se a edição quebrar a estrutura externa (ex: inserir uma chave que desalinha múltiplos itens), o compilador executa um full parse de recuperação.

---

## 5. Invariantes de Arquitetura

1. **Ausência de Dualidade Independente**: O CST é a única fonte da verdade sintática. Não existe um parser independente que opere sobre texto bruto sem passar pelo CST.
2. **Ponto e Vírgula Opcional em Linha Única**: Corpos de funções ou blocos de uma linha (`func ping(): int { return 0 }`) dispensam ponto e vírgula antes da chave de fechamento, garantindo ergonomia sem ambiguidades gramaticais.
3. **Pureza e Ausência de Efeitos no LSP**: A análise do CST e cálculo de tokens semânticos são queries Salsa puras executadas sobre snapshots imutáveis em workers de background, sem nunca segurar locks de escrita na thread principal do LSP.
