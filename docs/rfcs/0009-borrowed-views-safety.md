# RFC 0009: Segurança Estrutural de Fatias e Views Emprestadas (Borrowed Views)

- **Número da RFC:** 0009
- **Título:** Segurança Estrutural de Fatias ([]T) e Sub-strings Emprestadas
- **Autor(es):** Equipe do Compilador Arandu
- **Data de Início:** 2026-08-01
- **Status:** `Implemented` (Crates `arandu_middle`, `arandu_mir`, `stdlib`)
- **Área Principal:** `Memória` / `Stdlib` (`SL_S-Core`)
- **Substitui:** `docs/campaigns/sl-s-borrowed-views-gold-v0.1.md` (resgatada do histórico de commits)

---

## 1. Resumo (Summary)

Esta RFC define o contrato de segurança estática para **Views Emprestadas (*Borrowed Views*)** na linguagem Arandu, abrangendo fatias contíguas (`[]T`) e substrings (`ref str`). 

A proposta garante que qualquer projeção ou subfatia mantenha um vínculo estrito com a raiz proprietária do recurso (*root owner*), impedindo mutações ou desalocações concorrentes do contêiner enquanto houver fatias ativas, sem a necessidade de parâmetros sintáticos explícitos de lifetime (`'a`) ou contagem de referências oculta.

---

## 2. Motivação (Motivation)

Operações de fatiamento (*slicing*) em coleções como `Vec<T>` e strings são fontes notórias de corrupção de memória em linguagens sem segurança rigorosa:
* Se um programador obtém uma fatia `sub = vec[0..5]` e em seguida chama `vec.push(x)`, o vetor pode realocar seu buffer interno, transformando os elementos de `sub` em ponteiros pendentes (*dangling pointers*).
* Linguagens como C/C++ exigem disciplina manual falível.
* Rust resolve isso com parâmetros sintáticos de tempo de vida (`&'a [T]`), o que pode elevar significativamente a complexidade da assinatura de funções.

O Arandu resolve este desafio derivando **interfaces estruturais de empréstimo (*Structural Borrow Interfaces*)** diretamente da análise de fluxo da AMIR.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

No Arandu, funções que recebem e retornam fatias operam com sintaxe limpa e direta:

```arandu
func first_five(v: ref Vec<int>): []int {
    return v.slice(0, 5);
}

func main(): void {
    let mut numbers = Vec.new();
    numbers.push(10);
    numbers.push(20);

    let view = first_five(&numbers);

    // O compilador rastreia que 'view' empresta o buffer de 'numbers'.
    // A linha abaixo falha em compilação com o erro O003:
    // numbers.push(30); // ERRO: mutação enquanto 'view' está ativa!

    io.println(view[0]); // Uso seguro da fatia
}
```

---

## 4. Explicação em Nível de Referência: Resumo Estrutural de Retorno

O compilador analisa a função na AMIR e infere o contrato `ReturnBorrowSummary`:

```rust
pub struct ReturnBorrowSummary {
    pub dependencies: Vec<BorrowDependency>,
}

pub struct BorrowDependency {
    pub param_index: usize,
    pub projection_path: ProjectionPath,
}
```

* **Composição entre Funções**: Quando uma função chama `first_five`, o compilador substitui os parâmetros formais pelos argumentos reais passados na chamada, propagando a janela de empréstimo para o chamador de forma transitiva e monotônica.
* **Detecção de Conflitos**: Qualquer tentativa de invocar métodos consumidores ou mutáveis (`&mut self`) no proprietário enquanto o resultado do empréstimo estiver vivo dispara imediatamente os diagnósticos `O002` (*move-while-borrowed*) ou `O003` (*borrow-conflict*).

---

## 5. Invariantes de Arquitetura

1. **Sem Heurísticas Nominais**: O compilador nunca decide a segurança de uma chamada pelo nome do método (ex: checar se o método se chama `"push"` ou `"reserve"`). Toda decisão decorre estritamente da assinatura de tipos e do rvalue AMIR (`BorrowMut` vs `Borrow`).
2. **Deterministic Fixpoint**: A inferência de interfaces de empréstimo em funções recursivas ou chamadas circulares entre módulos utiliza um algoritmo por worklist que converge deterministicamente para o ponto fixo mínimo seguro.
