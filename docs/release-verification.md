# Verificação de releases Arandu

Uma release oficial contém três archives, seus sidecars por algoritmo,
`release-manifest.json`, `BLAKE3SUMS` e `SHA256SUMS`. O manifest liga versão,
tag, commit, target, tamanho e hashes de cada archive. O workflow rejeita um
arquivo ausente, extra ou divergente antes de criar uma release pública.

## Verificar os bytes

Baixe todos os assets da release e valide primeiro o checksum adequado ao host:

```bash
gh release download v0.1.0-rc.1 --repo arandu-lang/arandu --dir arandu-release
cd arandu-release
sha256sum --check SHA256SUMS
```

Quem possui `b3sum` pode validar também:

```bash
b3sum --check BLAKE3SUMS
```

Os arquivos `*.sha256`, `*.sha256sum`, `*.blake3` e `*.blake3sum` permanecem
publicados para instalação direta e ferramentas que esperam sidecars.

## Verificar origem e workflow

Checksum prova que os bytes correspondem ao manifest; provenance prova que os
bytes foram atestados pelo workflow do repositório. Verifique o archive e o
manifest, restringindo a identidade ao repositório e ao workflow de release:

```bash
gh attestation verify arandu-0.1.0-rc.1-x86_64-unknown-linux-gnu.tar.gz \
  --repo arandu-lang/arandu \
  --signer-workflow arandu-lang/arandu/.github/workflows/release.yml

gh attestation verify release-manifest.json \
  --repo arandu-lang/arandu \
  --signer-workflow arandu-lang/arandu/.github/workflows/release.yml
```

Para uma política ainda mais estrita, acrescente `--source-ref refs/tags/vX.Y.Z`
e compare `commit` no manifest com o commit imutável da tag.

## Publicação e correções

O workflow monta e valida o conjunto completo, cria uma release em draft,
anexa todos os assets, gera as attestations e somente então publica o draft.
Uma falha intermediária pode deixar um draft privado para inspeção, nunca uma
release pública parcial.

Antes da primeira RC oficial, habilite no GitHub:

1. `Settings` → `General` → `Releases`.
2. Selecione `Enable release immutability`.

A configuração vale apenas para releases futuras. Depois de publicada, uma
release imutável não permite substituir nem apagar tag/assets. Qualquer correção
de bytes exige nova versão e nova tag; nunca mova ou reutilize uma tag publicada.

## Dry-run

`Actions` → `Release` → `Run workflow` executa os mesmos builders, instalação,
smoke, conjunto exato, manifest e checksums. O resultado fica no artifact
`verified-release-set` do run. Execução manual não recebe permissões de
publicação, não cria release e não gera provenance oficial.
