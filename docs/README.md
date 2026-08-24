# Documentação do Arandu

Este índice separa fontes normativas de registros históricos. O histórico dos
arquivos removidos continua disponível no Git; um documento substituído não
permanece como stub na raiz.

## Fontes vivas

- [Roadmap mestre do compilador](arandu-compiler-roadmap-v0.1.md)
- [Campanha ativa: Project & Package Lifecycle Gold](arandu-project-package-lifecycle-gold-v0.1.md)
- [Arquitetura Salsa/LSP](arandu-salsa-lsp-architecture-v0.1.md)
- [Capacidades públicas do LSP/editor](arandu-lsp-capabilities-v0.1.md)
- [RFC GenRef Gold](arandu-genref-gold-rfc-v0.1.md)
- [Contrato de distribuição](arandu-distribution-contract-v0.1.md)
- [Verificação de releases](release-verification.md)
- [Especificação de diagnósticos](diagnostics/SPEC.md)
- [Contrato de nomes de anotações](arandu-attribute-naming-v0.1.md)

## Contratos técnicos

Os documentos `arandu-*-v0.1.md` que descrevem lexer, parser, AST, IR, ABI,
backends, stdlib, CLI/LSP e instrumentação são contratos da implementação.
Planos concluídos são consolidados no roadmap mestre e removidos da raiz.

## Diagnósticos e releases

- `errors/` contém uma página por `DiagCode`; essa bijeção é validada por
  `xtask check-diag-docs` e não é duplicação descartável.
- `releases/` contém a evidência de cada candidata publicada.

## Política de manutenção

1. Uma decisão tem uma única fonte normativa.
2. Pesquisa temporária vira decisão/risco/teste na fonte viva e depois é
   removida; não acumulamos notas soltas.
3. Um roadmap concluído é resumido no roadmap pai antes de ser removido.
4. Links relativos são verificados antes do merge.
