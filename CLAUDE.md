# Obscure Desktop — guia para a próxima sessão

Cliente Linux para Xray-core em Rust + GTK4 + libadwaita. **Fonte de verdade: `docs/PLANO.md`**
(identidade, stack, arquitetura, decisões, roadmap por fases e, no fim, o registro de progresso
com pendências). Não reabra decisões que estão lá.

## Convenções
- Responder em português do Brasil. Código, identificadores, comentários e commits em inglês.
- Commits pequenos, Conventional Commits. **Nunca fazer push.**
- UI sem jargão técnico, textos em pt-BR via gettext (`gettext("…")` no Rust, `_("…")` no Blueprint);
  `po/en.po` é o segundo idioma e deve ser mantido em dia.
- `obscure-core` nunca depende de GTK. Lógica testável vai para lá.
- Antes de encerrar uma fase: rodar build, testes e clippy de verdade, marcar itens no
  `docs/PLANO.md` e anotar pendências na seção 8 dele; atualizar este arquivo.

## Estrutura
```
Cargo.toml                    workspace (resolver 3, edition 2024, rust-version 1.92)
crates/obscure-core/          lib sem GTK (fase 1: links, profile, config, core_manager, supervisor, stats, sysproxy, subscription, latency)
crates/obscure-app/           app GTK: src/{main,application,window}.rs, src/ui/*.blp, build.rs, meson.build
crates/obscure-helper/        placeholder do helper privilegiado (fase 4)
data/                         .desktop/.metainfo/.gschema (*.in), icons/, resources/{gresource.xml,style.css,meson.build}
po/                           LINGUAS (en, pt_BR), POTFILES.in, en.po, pt_BR.po
build-aux/                    manifest Flatpak Devel (GNOME 50), cargo-sources.json, cargo.sh, dist-vendor.sh
.github/workflows/ci.yml      fmt + clippy + test + meson (Arch container) e Flatpak (gnome-50)
.githooks/pre-commit          fmt --check + clippy -D warnings (ativado por `meson setup` no perfil development)
```

## Build, rodar e testar
```bash
meson setup build -Dprofile=development   # app ID io.github.talesam.Obscure.Devel
meson compile -C build
./build/crates/obscure-app/obscure        # roda sem instalar (fallback para build/data no perfil development)
meson test -C build                       # cargo test + validação de desktop/metainfo/gschema

cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
- `config.rs`: meson gera `build/crates/obscure-app/config.rs` e passa `CODEGEN_BUILD_DIR`; sem meson,
  `build.rs` gera padrões de desenvolvimento. Constantes: `APP_ID, VERSION, PROFILE, GETTEXT_PACKAGE,
  LOCALEDIR, PKGDATADIR, BUILD_DATADIR`.
- Novo `.blp`: adicionar em `data/resources/meson.build` (lista de inputs), em
  `data/resources/resources.gresource.xml` e em `po/POTFILES.in`.
- Novas strings: `meson compile -C build obscure-pot && meson compile -C build obscure-update-po`,
  depois traduzir em `po/en.po`.
- Dependências novas no Cargo: regenerar `build-aux/cargo-sources.json` com
  `flatpak-cargo-generator.py Cargo.lock -o build-aux/cargo-sources.json`
  (script de flatpak-builder-tools; precisa de python-aiohttp e python-tomlkit).
- Flatpak: `flatpak-builder` não está instalado na máquina; `org.flatpak.Builder` (user) fornece
  `flatpak-builder-lint`: `flatpak run --command=flatpak-builder-lint org.flatpak.Builder manifest build-aux/io.github.talesam.Obscure.Devel.json`.
- Screenshot da janela para conferência visual (Wayland nega ScreenshotWindow): rodar com
  `GDK_BACKEND=x11`, achar a janela com `xdotool search --name '^Obscure$'` e capturar com `import -window <id> out.png`.

## Onde parou
Fase 0 concluída (commits locais na branch `main`). **Próximo passo: Fase 1 (obscure-core)**,
conforme `docs/PLANO.md` §5 — começar por `links.rs` (parser vless/vmess/trojan/ss com fixtures),
depois `profile.rs`, `config.rs`, `core_manager.rs`, `supervisor.rs`, `stats.rs`, `sysproxy.rs`,
`subscription.rs`, `latency.rs`. As dependências da fase 1 (tokio, serde, reqwest, tonic…) ainda não
foram adicionadas ao workspace; usar as versões listadas em `docs/PLANO.md` §2.2.
