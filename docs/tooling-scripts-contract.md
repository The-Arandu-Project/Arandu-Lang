# Contrato de ferramentas e scripts

> **Estado:** contrato vigente; a remoção de Python do fluxo de instalação é
> deliberadamente planejada para `DIST` na Fase 6 do roadmap.

Este é o mapa oficial de automação. Regras de compilação, resolução,
lockfile, integridade e release pertencem ao Rust (`xtask` ou crates). Scripts
não podem reimplementar essas regras; apenas adaptam o ambiente do sistema e
encaminham a validação para o comando canônico.

| Grupo | Arquivos | Responsabilidade |
| --- | --- | --- |
| Tarefas semânticas | `cargo run -p xtask -- ...` | Diagnósticos, corpus, release contract e campanhas do compilador |
| Empacotamento | `package-release.sh`, `package-release.ps1`, `prepare_release_assets.py` | Preparar staging e chamar validadores; diferenças de shell/runner |
| Integridade de archives | `reproducible_tar.py`, `reproducible_zip.py` | Formatos externos e testes adversariais; a política deve acompanhar o contrato Gold |
| Instalação | `install-from-tarball.sh`, `install-from-zip.ps1`, `install-local.sh` | SHA-256 bootstrap, staging, PATH e diretórios nativos |
| Smoke/reprodutibilidade | `smoke_distribution.py`, `test_release_archives.py`, `check-package-reproducibility.sh` | Exercitar artefatos públicos em runners nativos |
| Diagnósticos | `check-diag-docs.sh`, `check-diag-determinism.sh` | Wrappers de CI para `xtask` e testes de concorrência |

## Política por sistema

- Bash é o wrapper de Linux/macOS; PowerShell é o wrapper de Windows.
- Python é usado para testes e manipulação de formatos quando isso reduz a
  dependência de ferramentas externas; não decide identidade de dependência.
- O mesmo caso de uso deve produzir o mesmo resultado sem depender do shell.
  CI deve chamar o comando canônico diretamente quando houver equivalente.
- Um novo script exige dono, plataforma, comando canônico que valida sua saída
  e um teste. Duplicação de parser ou regra de segurança é proibida.

## Migração gradual

1. Adicionar a regra no crate/Rust responsável.
2. Cobrir a regra com teste unitário e um teste de integração multiplataforma.
3. Reduzir o script a preparação de argumentos e tradução de erros.
4. Manter o wrapper antigo durante uma versão e removê-lo somente após a CI
   usar o comando Rust em Windows, Linux e macOS.
