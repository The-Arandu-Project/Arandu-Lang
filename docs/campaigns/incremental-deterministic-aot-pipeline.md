# Campanha Ativa: Pipeline AOT Incremental e Determinístico

- **Status:** `Ativa — correção e medição`
- **Área:** `A1 / Backend / Tooling`
- **Data de Início:** 2026-09-12
- **Documento Normativo:** [RFC 0011](../rfcs/0011-incremental-partitioned-aot-and-in-process-linker.md)

## Escopo já implementado

O CLI persiste somente fingerprints verificáveis, nunca o grafo Salsa. O
cutoff integral cobre a closure de fontes, dependências, stdlib, compilador e
runtime, além do digest do executável. CGUs usam hash canônico e sidecars de
integridade; objetos órfãos são coletados acima de 500 MiB.

No Linux x86_64, o patch ELF é uma otimização fail-closed: exige função com
tamanho idêntico e relocações cuja resolução pode ser provada contra `.text`
ou uma entrada GOT já existente. Qualquer `.rodata`, seção, crescimento,
metadata inconsistente ou range não demonstrável usa o linker completo.

O harness `xtask bench-incremental` mede cinco classes de edição e funciona
como oráculo de SHA-256 entre paths e contagens de threads. A CI executa esse
oráculo; latência em runner compartilhado é observação, não threshold. O
relatório schema 2 também captura os timers do CLI por fase. A verificação do
digest do executável ocorre somente depois de toda a closure de inputs cortar,
evitando reler um artefato que será substituído quando um input já mudou.
Após um miss seguro, os fingerprints atuais já capturados são movidos para a
sessão substituta, sem reler compilador, runtime e stdlib; o digest produzido
pela publicação também é movido, sem um segundo hash do executável. Quando
todas as CGUs são hits, o executável anterior foi verificado e o layout ELF
prova igualdade exata da closure, ele é reutilizado sem invocar linker.

## Trabalho que mantém a campanha aberta

A meta original de alteração semântica ponta a ponta em até 10 ms não é
atendida. Uma edição de corpo ainda cria uma nova `DatabaseImpl` e recompõe o
frontend, embora preserve CGUs irmãs e use patch ELF quando seguro. A próxima
decisão arquitetural precisa comparar, com perfis representativos:

1. daemon local de build que conserva snapshots Salsa entre invocações;
2. formato persistente e versionado de resultados por item;
3. redução do custo de bootstrap sem enfraquecer a verificação integral de
   inputs e artefatos.

O relatório por fase atual mostra também que, no workload pequeno, o frontend
leva poucos milissegundos e a publicação ELF segura domina a edição de corpo:
copiar a imagem staging, sincronizá-la e recalcular seu digest ainda escala com
o tamanho total do executável. O marco `0.2` deve medir um protocolo recuperável
proporcional aos ranges alterados (ou clone CoW quando suportado), preservando o
CAS publicado e o fallback atômico portável; apenas manter Salsa quente não
cumpre sozinho o budget.

Não se deve marcar a RFC como `Implemented` nem reduzir as verificações de
integridade para obter números artificiais. Ao encerrar esta campanha, este
arquivo deve ser removido e as decisões consolidadas na RFC e no documento de
arquitetura permanente.

## Sequência aprovada no roadmap

- `0.1`: fundação batch verificável e funcional, já conectada a `arandu build`;
- `0.2`: serviço local supervisionado que mantém a DB Salsa quente, mas não a
  serializa, mais publicação dev proporcional à mudança e recuperável;
- `0.3`: lowering AMIR e codegen realmente demandados por instância, seguidos
  de CAS persistente apenas para representações canônicas externas ao Salsa.

Os gates e budgets desses marcos vivem na seção **Trilha incremental nativa —
RFC 0011** do roadmap mestre. Esta campanha permanece ativa enquanto o limite
program-wide de `lower_amir` e a DB fria entre processos existirem.
