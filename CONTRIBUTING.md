# Contribuindo com o Obscure

Obrigado pelo interesse! Leia `docs/PLANO.md` antes de propor mudanças: ele
registra as decisões de arquitetura e o roadmap.

## Regras rápidas

- **Idioma:** conversas, issues e textos de interface em português do Brasil.
  Código, identificadores, mensagens de commit e comentários em inglês.
- **Commits:** [Conventional Commits](https://www.conventionalcommits.org/)
  (`feat:`, `fix:`, `docs:`, `build:`, `ci:`, `refactor:`, `test:`, `chore:`).
  Commits pequenos e focados.
- **Qualidade:** antes de enviar, rode

  ```bash
  cargo fmt --all
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```

  O hook de pre-commit em `.githooks/` faz isso automaticamente. Ative com
  `git config core.hooksPath .githooks` (o `meson setup` no perfil de
  desenvolvimento faz isso para você).
- **Interface:** sem jargão técnico na tela principal. Textos de UI passam
  pelo gettext (`gettext("…")`), com pt-BR como idioma base e inglês em `po/en.po`.
- **Crates:** `obscure-core` nunca depende de GTK. Toda lógica que possa ser
  testada sem janela vai para lá.

## Build

Veja o `README.md` para os comandos de build com meson e Flatpak.

## Licença

Ao contribuir, você concorda que seu código seja distribuído sob a
GPL-3.0-or-later.
