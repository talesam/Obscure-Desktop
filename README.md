# Obscure Desktop

Cliente Linux moderno para o motor [Xray-core](https://github.com/XTLS/Xray-core), escrito em Rust com GTK4 + libadwaita.

**Conecte-se. Só isso.**

- Um botão Conectar. Sem jargão na tela principal.
- Importa links `vless://`, `vmess://`, `trojan://`, `ss://` e assinaturas colando ou por QR.
- Proxy de sistema (GNOME e KDE) por padrão, sem root. Modo túnel opcional com permissão via polkit.
- Baixa e atualiza o Xray-core sozinho, com verificação de hash.

Estado: **Fase 0 (esqueleto) concluída**. Roadmap e decisões em [`docs/PLANO.md`](docs/PLANO.md).

## Compilar e executar

Dependências: `rust` (>= 1.92), `gtk4` (>= 4.22), `libadwaita` (>= 1.9), `blueprint-compiler`,
`meson`, `ninja`, `gettext`, `desktop-file-utils`, `appstream`.

```bash
# Perfil de desenvolvimento (app ID io.github.talesam.Obscure.Devel, binário roda sem instalar)
meson setup build -Dprofile=development
meson compile -C build
./build/crates/obscure-app/obscure

# Testes: unitários do cargo + validação de .desktop, metainfo e gschema
meson test -C build

# Instalação (perfil padrão, release)
meson setup build-release --prefix=/usr
meson compile -C build-release
sudo meson install -C build-release
```

Só Rust, sem meson (útil para IDEs e para `cargo test`/`cargo clippy`):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Flatpak

```bash
flatpak install flathub org.gnome.Platform//50 org.gnome.Sdk//50 \
  org.freedesktop.Sdk.Extension.rust-stable//25.08
flatpak-builder --user --install --force-clean build-flatpak \
  build-aux/io.github.talesam.Obscure.Devel.json
flatpak run io.github.talesam.Obscure.Devel
```

Depois de mudar dependências no `Cargo.toml`, regenere as fontes vendorizadas:

```bash
python3 flatpak-cargo-generator.py Cargo.lock -o build-aux/cargo-sources.json
```

(`flatpak-cargo-generator.py` vem de
[flatpak-builder-tools](https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo)
e precisa de `python-aiohttp` e `python-tomlkit`.)

## Estrutura

| Diretório | Conteúdo |
|---|---|
| `crates/obscure-core` | Biblioteca sem GTK: links, assinaturas, config do Xray, supervisor, stats, proxy de sistema |
| `crates/obscure-app` | Aplicativo GTK4/libadwaita (`.blp` em `src/ui/`) |
| `crates/obscure-helper` | Helper privilegiado para o modo túnel (fase 4) |
| `data/` | `.desktop`, metainfo, gschema, ícones, recursos (CSS, gresource) |
| `po/` | Traduções (`pt_BR` é o idioma base, `en` é o segundo) |
| `build-aux/` | Manifest Flatpak, `cargo-sources.json`, scripts de build |


Licença: [GPL-3.0-or-later](LICENSE).
