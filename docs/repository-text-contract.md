# Contrato de Texto do Repositório

**Status:** implementado e exigido pelo S0 Gate

## Visão Geral e Contexto

Todo texto versionado no Arandu usa UTF-8 sem BOM e LF, independentemente do
sistema operacional ou da configuração global do Git. O contrato elimina
mudanças incidentais de offsets, manifests com `\r` e snapshots divergentes
entre Windows, Linux e macOS.

## Detalhes Técnicos da Implementação

- `.gitattributes` normaliza texto para LF no index e marca assets binários.
- `.editorconfig` solicita UTF-8, LF e newline final aos editores.
- `xtask check-line-endings` consulta `git ls-files --eol -z` e rejeita blobs
  `i/crlf` ou `i/mixed`; portanto, o resultado não depende do checkout local.
- O S0 Gate roda essa verificação antes da compilação e dos testes dourados.
- `git add --renormalize .` é a migração canônica para clones que precedem o
  contrato. O diff deve ser revisado normalmente, especialmente em fixtures.

Arquivos binários devem receber regra explícita em `.gitattributes`. Regras
específicas de fixtures podem preservar whitespace, mas não finais CRLF no
index salvo decisão expressa do contrato testado.

## PONTOS DE MELHORIA (O que não está no roadmap)

- O verificador depende do executável Git, o que é aceitável no checkout de
  desenvolvimento e no CI. Ele não faz parte do SDK instalado.
- O check valida encoding/newline do index por metadados Git; não tenta ser um
  linter geral de Unicode, Markdown ou whitespace semântico.

## Futuro e Próximos Passos

- Adicionar regras binárias quando novos formatos entrarem no repositório.
- Manter o check no início do S0 Gate para que uma quebra seja curta e
  diagnóstica, antes de snapshots ou empacotamento falharem indiretamente.

### Referências

- [Git `gitattributes`](https://git-scm.com/docs/gitattributes/2.50.0)
- [GitHub: configuring line endings](https://docs.github.com/en/get-started/git-basics/configuring-git-to-handle-line-endings)
- [EditorConfig specification](https://spec.editorconfig.org/)
