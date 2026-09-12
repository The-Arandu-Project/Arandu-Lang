# RFC 0004: Ciclo de Vida de Projetos e Manifesto Arandu.toml

- **Número da RFC:** 0004
- **Título:** Ciclo de Vida de Pacotes, Manifesto `Arandu.toml` e Resolução Determinística de Dependências
- **Autor(es):** Equipe do Compilador Arandu
- **Data de Início:** 2026-09-11
- **Status:** `Implemented`
- **Área Principal:** `Tooling` / `CLI` / `Query` (`arandu_cli`, `arandu_query`)

---

## 1. Resumo (Summary)

Esta RFC define o contrato formal do gerenciamento de projetos e pacotes do Arandu. Estabelece a especificação do manifesto `Arandu.toml`, o formato canônico do `Arandu.lock`, as garantias de compilação reproduzível, o isolamento do grafo de dependências e a separação estrita entre efeitos de sistema de arquivos e queries semânticas puras do Salsa.

---

## 2. Motivação (Motivation)

Linguagens modernas necessitam de uma experiência unificada de ferramentas (*tooling*) para inicializar, compilar, testar e empacotar aplicações sem exigir scripts ad-hoc de compilação ou conhecimento dos componentes internos do compilador. Ao mesmo tempo, ferramentas de build que executam código arbitrário durante a resolução de dependências introduzem brechas de segurança na cadeia de suprimentos (*supply-chain vulnerabilities*) e quebram a reproducibilidade.

O Arandu resolve esses problemas através de um manifesto estático puramente declarativo (TOML 1.0) validado por esquema estrito, e um lockfile determinístico orientado a hash BLAKE3.

---

## 3. Comandos do Ciclo de Vida de Projetos

O `arandu` provê os comandos canônicos para ciclo de vida:

```bash
arandu new <project-name> [--bin|--lib] [--vcs=auto|git|none]
arandu init [--bin|--lib]
arandu build [--release]
arandu check
arandu test
arandu bench
arandu clean
```

---

## 4. Estrutura Canônica do Manifesto (`Arandu.toml`)

```toml
[package]
name = "meu_projeto"
version = "0.1.0"
edition = "2026"
authors = ["Autor <autor@uneb.br>"]
license = "MIT OR Apache-2.0"

[dependencies]
collections = { path = "../libs/collections" }

[profile.release]
opt_level = 2
```

---

## 5. Invariantes de Arquitetura e Efeitos

1. **Separação I/O vs Salsa**: O parsing do `Arandu.toml` é puramente sintático. A leitura do disco, detecção de Git/VCS e criação de diretórios pertencem exclusivamente ao `arandu_cli`. Valores entram na base incremental Salsa apenas como estruturas canônicas hash-estáveis (`PackageModuleMap`, `DirectoryListing`).
2. **Lockfile Canônico e Bit-Identidade**: O arquivo `Arandu.lock` é gerado ordenado alfabeticamente, serializado em UTF-8 sem BOM com quebras de linha LF, garantindo hash BLAKE3 idêntico em Windows, Linux e macOS.
3. **Hermeticidade Offline**: Comandos `check`, `build` e `test` operam 100% offline a partir de artefatos cacheados e verificados, sem chamadas de rede ocultas no caminho quente de compilação.
