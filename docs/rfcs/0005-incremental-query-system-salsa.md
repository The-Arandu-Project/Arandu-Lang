# RFC 0005: Sistema de Queries Incrementais (Salsa) para o Compilador Arandu

- **Número da RFC:** 0005
- **Título:** Sistema de Queries Incrementais (Salsa) e o 6º Invariante de Identidade Única
- **Autor(es):** Equipe do Compilador Arandu
- **Data de Início:** 2026-07-05
- **Status:** `Implemented` (Crate `arandu_query`)
- **Área Principal:** `Incrementalidade` / `Compiler Infrastructure` (`A1`)
- **Substitui:** `docs/arandu-salsa-architecture-rfc.md` (resgatada do histórico de commits)

---

## 1. Resumo (Summary)

Esta RFC estabelece a arquitetura do motor incremental orientado a demanda do Arandu utilizando a biblioteca **Salsa**. O compilador substitui caches pontuais ad-hoc de arquivos (`ParseCache`) por um grafo acíclico dirigido (DAG) de dependências reativas, garantindo early-cutoff e recomputação mínima durante edições no editor de código (LSP) e compilação contínua (`arandu watch`).

A RFC formaliza o **6º Invariante Arquitetural do Arandu**: o Salsa nunca introduz um sistema paralelo de identificadores; ele opera estritamente sobre os IDs canônicos do compilador (`FileId`, `SymbolId`, `BlockId`, `TypeId`).

---

## 2. Motivação (Motivation)

Sistemas tradicionais de compilação em lote (*batch compilation*) não escalam para a experiência interativa moderna exigida por IDEs:
1. **Multi-módulo real**: Editar uma única linha em um módulo forçava a reanálise completa de todo o grafo de dependências transitivas.
2. **Latência de IDE sub-100ms**: Sem invalidação fina orientada a query, cada digitação (*keystroke*) exigia reparse e re-checagem de tipos integral do arquivo, causando engasgos na UI do editor.
3. **Duplicação de Análise de Fluxo**: Cálculos de *liveness* e análise de empréstimos eram historicamente recomputados de forma independente pelo borrow checker e pelo backend gerador de código.

---

## 3. O 6º Invariante Arquitetural: Identidade Única sob Incrementalidade

> **Invariante 6**: O Salsa nunca introduz um sistema de identidade paralelo ao já existente no compilador. Toda query `#[salsa::input]` e `#[salsa::tracked]` utiliza como chave diretamente os IDs nativos do Arandu (`FileId`, `SymbolId`, `BlockId`, `TypeId`). O Salsa atua puramente como uma camada transparente de memoização e corte de invalidação sobre estruturas existentes — jamais como dono de uma nova identidade que exigiria tabelas de tradução bidirecionais.

---

## 4. Camada de Input e Grafo de Dependências

O único ponto de entrada de texto no motor incremental é o input Salsa do arquivo-fonte:

```rust
#[salsa::db]
pub trait ArandCompilerDb: salsa::Database {
    fn source_text(&self, file: FileId) -> Arc<str>;
    fn file_path(&self, file: FileId) -> Arc<PathBuf>;
}

#[salsa::input]
pub struct SourceFile {
    pub file_id: FileId,
    #[return_ref]
    pub text: Arc<str>,
}
```

### Durabilidade

Arquivos da biblioteca padrão (`stdlib`) e dependências travadas em `vendor` recebem `salsa::Durability::HIGH`, garantindo que edições corriqueiras no código do usuário (`Durability::LOW`) não invalidem a árvore tipada da biblioteca padrão.

---

## 5. Granularidade Fina por Bloco Básico na AMIR

Enquanto ferramentas como *rust-analyzer* tradicionalmente memoizam no nível de função ou item, o Arandu estende o early-cutoff até o nível de **Bloco Básico (`BlockId`)** no fluxo de dados da AMIR:

```rust
#[salsa::tracked]
fn block_dataflow_facts(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    func_sym: SymbolId,
    block: BlockId,
) -> DataflowFacts {
    let func = func_amir(db, file, func_sym);
    let bb = func.block(block);

    let pred_facts: Vec<DataflowFacts> = predecessors(&func, block)
        .map(|pred_id| block_dataflow_facts(db, file, func_sym, pred_id))
        .collect();

    compute_transfer(bb, &pred_facts)
}
```

* **Impacto Prático**: Se o programador alterar uma instrução no bloco `bb3` de uma função contendo 12 blocos, apenas `bb3` e os blocos dependentes alcançáveis em RPO (*Reverse Post-Order*) recomputam o dataflow. Os blocos predecessores e irmãos disjuntos permanecem intactos.

---

## 6. Liveness Compartilhada entre Borrow Checker e Backend

A mesma query de *liveness* (`liveness_facts`) atende simultaneamente dois consumidores vitais:
1. **Borrow Checker**: Determina com precisão a janela de empréstimo (*loan window*) de uma referência a partir da *live range* do valor SSA.
2. **Backend Cranelift**: Alimenta o alocador de registradores.

Ambos os subsistemas consomem exatamente o mesmo resultado memoizado em memória, eliminando divergências de estado e trabalho computacional redundante.

---

## 7. Determinismo de Diagnósticos sob Paralelismo

Como o motor Salsa pode avaliar queries puras concorrentemente em threads distintas, a ordem de emissão de diagnósticos deve ser formalmente estabilizada antes de qualquer apresentação ao usuário:

```rust
pub fn finalize_diagnostics(mut diags: Vec<Diagnostic>) -> Vec<Diagnostic> {
    diags.sort_by(|a, b| {
        a.span.file_id.cmp(&b.span.file_id)
            .then(a.span.start.cmp(&b.span.start))
            .then(a.code.cmp(&b.code))
    });
    diags
}
```
Isso garante a propriedade `DET` do compilador: saídas e relatórios idênticos bit a bit independentemente do número de núcleos de CPU utilizados no host de compilação.
