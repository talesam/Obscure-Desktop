# Obscure Desktop — Plano do projeto

> Cliente Linux **moderno, simples e bonito** para o motor **Xray-core**, escrito em **Rust + GTK4 + libadwaita**.
> Referência do que NÃO queremos: Throne (Qt, dezenas de opções técnicas, core setuid root).
> Referência do que queremos: Hiddify (um botão Conectar), Snapshot/Loupe (visual GNOME), oxidom (GUI sem root).
>
> Pesquisa feita em 2026-09-19. Este documento é a fonte de verdade das decisões; atualize-o quando algo mudar.

---

## 1. Identidade

| Item | Decisão |
|---|---|
| Nome | **Obscure** (exibido) / repositório `Obscure-Desktop` |
| App ID | `io.github.talesam.Obscure` (reverse-DNS aceito pelo Flathub sem precisar de domínio próprio; trocar antes do primeiro commit se preferir outro) |
| Binário | `obscure` |
| Licença | **GPL-3.0-or-later** (padrão do ecossistema GNOME; como o Xray roda em processo separado, não há contaminação de licença em nenhuma direção) |
| Idioma padrão | **Inglês como idioma-fonte** (msgid) e traduções gettext para 28 idiomas: bg, cs, da, de, el, es, et, fi, fr, he, hr, hu, is, it, ja, ko, nl, nb, pl, pt, pt_BR, ro, ru, sk, sv, tr, uk, zh (decisão de 2026-09-20; `nb` substitui `no` e `zh` cobre zh_CN/zh_TW por fallback do gettext) |
| Slogan | "Conecte-se. Só isso." |

Princípios de UX (inegociáveis):
1. **Tela principal sem jargão.** Nada de "REALITY", "XTLS-Vision", "SNI", "outbound" na tela inicial. Isso fica em *Avançado*.
2. **Um botão.** Conectar/Desconectar é a ação central. Tudo o mais é secundário.
3. **Funciona sem root.** A GUI nunca roda privilegiada. Modo padrão é proxy de sistema. TUN é opcional e pede permissão uma vez, de forma limpa (polkit).
4. **Nunca deixa o sistema quebrado.** Proxy de sistema é restaurado ao sair, ao travar e no próximo início (estado anterior salvo em disco).
5. **Erros em português humano.** "O servidor não respondeu" em vez de stack trace do Xray. Log bruto disponível, mas escondido.
6. **Adaptativo.** Funciona de 360 px a 4K (AdwBreakpoint), tema claro/escuro/accent do sistema.

---

## 2. Stack (versões verificadas em 2026-09-19)

### 2.1 Ambiente local
- GTK 4.22.4, libadwaita 1.9.3, blueprint-compiler 0.22.2, meson 1.12, rustc 1.98.1, polkit 127.
- Flathub: `org.gnome.Platform//50` e `//51` disponíveis; `org.freedesktop.Sdk.Extension.rust-stable//26.08`.
- sing-box 1.14 em `extra` (não usaremos; ver §4.3). Xray **não** está nos repositórios oficiais do Arch → o app baixa o core sozinho.

### 2.2 Crates principais
```toml
[dependencies]
gtk  = { package = "gtk4",       version = "0.11", features = ["v4_22"] }   # 0.11.4
adw  = { package = "libadwaita", version = "0.9",  features = ["v1_9"] }    # 0.9.2
glib = "0.22"; gio = "0.22"
tokio = { version = "1", features = ["rt-multi-thread", "process", "sync", "time", "io-util"] }
async-channel = "2"
serde = { version = "1", features = ["derive"] }; serde_json = "1"
reqwest = { version = "0.13", default-features = false, features = ["rustls-no-provider", "json", "stream"] }  # + rustls 0.23 com provider ring (evita aws-lc/cmake)
tonic = "0.14"; prost = "0.14"           # gRPC StatsService do Xray (build: tonic-build)
url = "2"; base64 = "0.22"; percent-encoding = "2"; uuid = "1"
tracing = "0.1"; tracing-subscriber = "0.3"
gettext-rs = { version = "0.8", features = ["gettext-system"] }
ashpd = { version = "0.13", features = ["gtk4", "tokio"] }   # portais: Background/autostart, Screenshot (QR)
oo7 = "0.6"                                                  # segredos (Secret Service / portal)
zbus = { version = "5", features = ["tokio"] }              # D-Bus (KDE reparse, helper TUN)
ksni = "0.3"                                                 # tray SNI (opcional; GNOME exige extensão)
qrcode = "0.14"; rqrr = "0.11"; image = "0.25"              # gerar / ler QR
sha2 = "0.10"; zip = "2"                                     # verificar e extrair Xray
```
Sem crate maduro para config Xray tipada ou `subscription-userinfo`: código próprio em `obscure-core`.
`vpn-link-serde 0.1.5` (MIT) serve de referência para o parser de links, mas escrevemos o nosso (controle total + testes).

### 2.3 Arquitetura de UI: gtk-rs puro + composite templates + Blueprint
Descartado Relm4 (camada extra, macro própria, atraso em relação ao gtk-rs). Padrão usado por Fractal, Loupe, Snapshot:
- `glib::wrapper!` + `#[derive(glib::Properties)]` + `#[template(resource=…)]` + `#[template_callback]`.
- UI declarativa em `.blp` → `.ui` via meson; CSS em `style.css` com `@media (prefers-color-scheme: dark)`.
- Estado em GObjects (`ServerObject`, `SubscriptionObject`, `TrafficStats`) dentro de `gio::ListStore`; `gtk::ListView` + `BuilderListItemFactory`; `FilterListModel`/`SortListModel` para busca e ordenação por latência.
- Preferências em `gio::Settings` (gschema) com `bind()`.
- Async: tudo que toca UI em `glib::spawn_future_local`; I/O (processo do Xray, HTTP, gRPC) num runtime **tokio** em thread própria; ponte por `async-channel`.

### 2.4 Widgets libadwaita que vamos usar (e para quê)
| Widget | Uso no Obscure |
|---|---|
| `AdwNavigationSplitView` + `AdwSidebar` (1.9) | Sidebar de servidores/grupos, colapsa em telas estreitas |
| `AdwBreakpoint` / `AdwMultiLayoutView` | Layout adaptativo |
| `AdwToolbarView` | Header + barra inferior de estatísticas |
| `AdwStatusPage` + botão `.circular.suggested-action` ~128 px | Hero "Conectar" (modelo: Snapshot) |
| `AdwSpinner` | Estado "conectando" |
| `AdwToggleGroup` (1.7) | Modo de rota: Tudo / Inteligente / Só bloqueados |
| `AdwBottomSheet` (1.6) | Painel de log e de detalhes do servidor |
| `AdwDialog` + `AdwEntryRow` | Importar link / assinatura |
| `AdwAlertDialog` | Confirmações (remover, conceder permissão TUN) |
| `AdwPreferencesDialog` | Configurações (com busca, 1.9) |
| `AdwToast` | Feedback (importado, copiado, erro) |
| `AdwWrapBox` (1.7) | Chips de protocolo/transporte no detalhe |
| `AdwShortcutsDialog` (1.8) | Atalhos |
| `AdwAboutDialog` | Sobre |

---

## 3. Arquitetura do sistema

```
┌──────────────────────────────────────────────────────────────┐
│  obscure (GTK4/libadwaita, sem privilégios)                  │
│   ui/ ── ViewModel (GObjects) ── obscure-core (lib Rust)     │
│                                     │  tokio thread          │
│        ┌────────────────────────────┼─────────────────────┐  │
│        │ CoreManager: baixa/verifica/atualiza o Xray       │  │
│        │ ConfigBuilder: Perfil → config.json               │  │
│        │ Supervisor: spawn `xray run -c` + stdout/stderr   │  │
│        │ Stats: gRPC StatsService (tonic) a cada 1 s       │  │
│        │ SysProxy: gsettings (gio) + kioslaverc + D-Bus    │  │
│        │ Subscriptions: HTTP + base64 + userinfo           │  │
│        │ Latency: TCP handshake / URL test via SOCKS       │  │
│        └───────────────────────────────────────────────────┘  │
└──────────────┬─────────────────────────────┬─────────────────┘
               │ subprocess + gRPC 127.0.0.1 │ D-Bus (fase TUN)
      ┌────────▼────────┐          ┌─────────▼───────────────┐
      │ xray (MPL/GPL)  │          │ obscure-helper (root via │
      │ ~/.local/share/ │          │ polkit): setcap / TUN fd │
      │ obscure/core/   │          └─────────────────────────┘
      └─────────────────┘
```

### 3.1 Workspace Cargo
```
Obscure-Desktop/
├── Cargo.toml                 # workspace
├── meson.build / meson_options.txt
├── build-aux/
│   ├── io.github.talesam.Obscure.Devel.json   # Flatpak manifest (GNOME 50 → 51)
│   ├── cargo-sources.json                     # flatpak-cargo-generator
│   └── dist-vendor.sh
├── crates/
│   ├── obscure-core/          # lib pura, sem GTK: links, subs, config, supervisor, stats, sysproxy
│   ├── obscure-helper/        # binário privilegiado mínimo (fase 4)
│   └── obscure-app/           # GTK app (main.rs, application.rs, window.rs, ui/*.rs, ui/*.blp)
├── data/
│   ├── icons/hicolor/{scalable,symbolic}/apps/
│   ├── resources/ (style.css, gresource.xml)
│   ├── io.github.talesam.Obscure.desktop.in.in
│   ├── io.github.talesam.Obscure.metainfo.xml.in.in
│   ├── io.github.talesam.Obscure.gschema.xml.in
│   └── io.github.talesam.Obscure.policy.in       # polkit (fase 4)
├── po/  (POTFILES.in, LINGUAS, pt_BR.po, en.po)
├── packaging/arch/PKGBUILD
├── docs/PLANO.md  (este)
└── .github/workflows/ (ci.yml: fmt+clippy+test; flatpak.yml)
```
`obscure-core` não depende de GTK: testável com `cargo test`, reutilizável numa CLI (`obscure-cli`) no futuro.

### 3.2 Dados em disco (XDG, compatível com Flatpak)
- `~/.config/obscure/profiles.json` — perfis, grupos, assinaturas (serde; migração por campo `version`).
- `~/.local/share/obscure/core/{xray, geoip.dat, geosite.dat, VERSION}` — core baixado, SHA-256 verificado via `.dgst`.
- `~/.local/share/obscure/state.json` — proxy de sistema anterior (para restauração) e último perfil.
- `~/.cache/obscure/logs/xray.log` — rotação simples.
- Segredos (senhas de assinatura com auth) → `oo7`.

### 3.3 Gerenciamento do Xray-core
- Versionamento por data (`v26.3.27` estável "latest"; pré-releases 26.9.x). Padrão: **estável**; toggle "canal pré-release" em Avançado.
- Download de `https://api.github.com/repos/XTLS/Xray-core/releases/latest` → asset `Xray-linux-64.zip` (x86_64) / `Xray-linux-arm64-v8a.zip` (aarch64) + `.dgst`. Verificar SHA-256 antes de extrair.
- Geo: `Loyalsoldier/v2ray-rules-dat` (`geoip.dat`, `geosite.dat`, releases diárias) via `releases/latest/download/…` + `.sha256sum`. Checagem 1×/dia.
- Execução: `xray run -c <config>` com `XRAY_LOCATION_ASSET` apontando para o dir de dados. Validação prévia com `xray run -test`. Stdout/stderr lidos em stream para o painel de log.
- gRPC: `api.listen = 127.0.0.1:<porta aleatória>` com `StatsService` (+ `LoggerService`). Protos vendorizados de `app/stats/command/command.proto` e dependências (`common/serial/typed_message.proto`), compilados com `tonic-build`. Contadores `outbound>>>proxy>>>traffic>>>uplink|downlink`.

### 3.4 Config gerada (modo proxy, padrão)
- Inbound `mixed` em `127.0.0.1:2080` (porta configurável), `udp: true`, sniffing `http,tls,quic`.
- Outbounds `proxy` (do perfil), `direct` (freedom), `block` (blackhole).
- DNS: DoH `https://1.1.1.1/dns-query` por padrão (configurável).
- Rota conforme preset:
  - **Tudo**: tudo → proxy, exceto `geoip:private`.
  - **Inteligente** (padrão): `geoip:private` → direct; `geosite:category-ads-all` → block; resto → proxy.
  - **Só bloqueados**: lista de domínios do usuário → proxy; resto → direct.
- `stats` + `policy.system.statsOutbound*` ligados.
Suporte de protocolos (fase 1): **VLESS** (raw/ws/grpc/xhttp/httpupgrade; tls/reality; flow vision), **VMess**, **Trojan**, **Shadowsocks** (incl. 2022). Fase 3: WireGuard, Hysteria2 (Xray suporta cliente Hy2), SOCKS/HTTP upstream.

### 3.5 Share links e assinaturas
- Parser próprio, testado com fixtures reais: `vless://` (padrão XTLS #716, inclusive `pbk/sid/spx/fp/sni/alpn/mode/extra/pqv/fm`), `vmess://` (base64 JSON v2rayN), `trojan://`, `ss://` (SIP002 base64url **e** plain percent-encoded para 2022, mais legado base64 inteiro).
- Assinatura: GET com User-Agent `Obscure/<versão> (Linux)`; corpo base64 ou plain, um link por linha; JSON XTLS (#4877) aceito também. Headers: `subscription-userinfo` (upload/download/total/expire), `profile-title`, `profile-update-interval`, `profile-web-page-url`, `support-url`. 301–308 e `moved-permanently-to` atualizam a URL.
- Entrada: colar (Ctrl+V global na janela detecta link/URL no clipboard), diálogo, arquivo `.json`/`.txt`, QR via portal Screenshot (`ashpd` + `rqrr`), deep link `obscure://` e handler de `vless://`, `vmess://`, `trojan://`, `ss://` no `.desktop`.
- Saída: copiar link, mostrar QR (`qrcode` → `gdk::Texture`).

### 3.6 Proxy de sistema (padrão)
- GNOME/Cinnamon/XFCE: `gio::Settings` em `org.gnome.system.proxy{,.http,.https,.socks}` (mode manual, host/port, `ignore-hosts`). **Sem spawn de `gsettings`.**
- KDE: escrever `~/.config/kioslaverc` (`ProxyType=1`, `httpProxy=http://127.0.0.1 2080`, `socksProxy=…`, `NoProxyFor`) via `kwriteconfig6|5` e sinal D-Bus `org.kde.KIO.Scheduler.reparseSlaveConfiguration`.
- **Sempre escrever os dois** (lição Throne #1017). Salvar estado anterior em `state.json`; restaurar em desconectar, no `shutdown` do GApplication e no próximo start se detectar estado sujo (crash).
- Botão "Copiar variáveis de ambiente" (`export http_proxy=…`) para terminais.
- Referência: `sysproxy-rs` (Clash Verge Rev).

### 3.7 Modo TUN (Avançado, fase 4)
- Usar o **inbound `tun` nativo do Xray** (desde v26.1.23): `gateway`, `autoSystemRoutingTable: ["0.0.0.0/0","::/0"]`, `autoOutboundsInterface: "auto"`. Dispensa sing-box e tun2socks.
- Privilégio, em dois níveis:
  1. **Simples (primeiro):** botão "Conceder permissão" → `pkexec setcap cap_net_admin,cap_net_raw,cap_net_bind_service+ep <xray>` com arquivo `.policy` próprio. Reaplicado automaticamente após atualizar o core. Funciona em instalação nativa (Arch/AUR).
  2. **Robusto (depois):** `obscure-helper` como serviço D-Bus de sistema ativado por polkit, que abre `/dev/net/tun`, configura rotas e passa o fd via `XRAY_TUN_FD`. É o único caminho viável para Flatpak (via `flatpak-spawn --host` ou serviço pré-instalado) e AppImage (`nosuid`).
- Nunca setuid root (Throne). Nunca rodar a GUI como root (Hiddify).
- Regra de UI: TUN e Proxy de sistema são **um seletor** ("Como aplicar: Proxy de sistema | Túnel (todo o tráfego)"), não dois toggles independentes que conflitam.

### 3.8 Latência
- Teste rápido: TCP handshake até `host:port` (ms), colorido verde < 150, amarelo < 400, vermelho acima; sem tocar no Xray.
- Teste real (opcional, "Testar conexão"): sobe instância temporária do Xray com inbound socks em porta aleatória e faz `HEAD https://www.gstatic.com/generate_204` via `reqwest::Proxy`.
- "Testar todos" com concorrência limitada (8), ordenação por resultado, "Auto: melhor servidor" como perfil virtual.

### 3.9 Tray, autostart, notificações
- Tray via `ksni` (SNI), **opcional** e desligado se não houver host SNI (GNOME sem extensão). Ícone espelha estado; menu: Conectar/Desconectar, trocar servidor, Sair.
- Autostart via portal Background (`ashpd`), com flag `--start-minimized`/`--connect`.
- Notificações via `gio::Notification` (funciona em Flatpak).
- Single-instance pelo próprio GApplication (`HANDLES_OPEN` para links).

---

## 4. Decisões e alternativas descartadas

| Decisão | Alternativa descartada | Motivo |
|---|---|---|
| gtk-rs puro + Blueprint | Relm4 | Sem camada extra; padrão GNOME; docs 1:1 |
| Xray como **subprocesso** | Embutir via FFI/cgo | Licença limpa, atualização independente, crash isolado |
| TUN nativo do Xray | sing-box (Throne), tun2socks, hev-socks5-tunnel | Um binário só; sing-box tem cláusula de marca e é GPL; menos peças |
| setcap via pkexec → depois helper D-Bus | setuid root (Throne), rodar como root (Hiddify), senha em campo (v2rayN) | Segurança e UX |
| JSON próprio em `~/.config` | SQLite (Throne) | Volume pequeno; diff legível; backup trivial |
| Proxy de sistema como padrão | TUN como padrão | Não exige privilégio; funciona em Flatpak |
| GPL-3.0-or-later | MIT | Coerência GNOME; permite estudar ideias de Throne (sem copiar código) |

---

## 5. Roadmap por fases

### Fase 0 — Esqueleto (1 semana) — **concluída em 2026-09-19**
- [x] Workspace Cargo + meson + Blueprint (base: gtk-rust-template do GNOME World, adaptado para workspace).
- [x] `AdwApplicationWindow` vazia com `AdwToolbarView`, about dialog, gschema, `.desktop`, metainfo, ícone provisório.
- [x] Flatpak manifest Devel (GNOME 50; migrar para 51 assim que a imagem CI existir), `cargo-sources.json`.
- [x] CI: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, build Flatpak.
- [x] `README.md`, `CONTRIBUTING.md`, `LICENSE`, `.editorconfig`, hook pre-commit.
**Critério:** `meson setup build && meson compile -C build && ./build/…/obscure` abre a janela; Flatpak builda.
Verificado localmente: janela abre, `meson test` (4/4), `cargo test`, `cargo clippy -D warnings` e `cargo fmt --check` passam. O build Flatpak só foi validado com `flatpak-builder-lint` (ver §8).

### Fase 1 — obscure-core (2 semanas)
- [x] `links.rs`: parse/serialize vless/vmess/trojan/ss + testes com fixtures.
- [x] `profile.rs`: modelo de dados (Profile, Group, Subscription) + persistência versionada. *(grupos/assinaturas ficam para a Fase 3)*
- [x] `config.rs`: Profile + RoutePreset → `config.json` (usa `serde_json::json!`); testes de asserção.
- [x] `core_manager.rs`: detectar arquitetura, baixar, verificar `.dgst`, extrair, atualizar; geo data. *(geo vem do zip do Xray; atualização diária via Loyalsoldier pendente)*
- [x] `supervisor.rs`: spawn/kill do Xray, `-test` antes de subir, stream de log. *(reinício com backoff pendente)*
- [ ] `stats.rs`: tonic client do `StatsService`, polling 1 s, taxa e acumulado da sessão.
- [x] `sysproxy.rs`: GNOME + KDE, salvar/restaurar estado.
- [ ] `subscription.rs`: fetch, decode, userinfo, intervalo.
- [ ] `latency.rs`: TCP + URL test.
**Critério:** teste de integração que importa um `vless://`, sobe o Xray, faz um request via SOCKS e lê stats.
**Estado (2026-09-20):** `crates/obscure-core/tests/integration.rs` importa o link, sobe o Xray real e faz requests via SOCKS e HTTP (passou com servidor REALITY real). Faltam `stats.rs`, `subscription.rs`, `latency.rs`.

### Fase 2 — UI MVP (2–3 semanas)
- [ ] Janela: sidebar (servidores/grupos, busca, latência colorida) + hero Conectar + rodapé com ↑/↓ e tempo conectado.
  - Estado vazio (sem servidor): o hero mostra **"Adicionar servidor"**, nunca "Conectar". O botão Conectar só existe quando há pelo menos um servidor (já aplicado no esqueleto da Fase 0).
- [x] Importar: colar (Ctrl+V) e diálogo com validação ao vivo. Toast de resultado. *(arquivo e handler de URL scheme pendentes)*
- [ ] Detalhe do servidor em bottom sheet: nome editável, chips (protocolo, transporte, segurança), QR, copiar link, remover.
- [x] Seletor "Como aplicar" (`AdwToggleGroup`: Proxy do sistema padrão, Só proxy local, Túnel desativado). *(preset de rota na UI pendente; o core já suporta)*
- [ ] Painel de log (bottom sheet, monoespaçado, filtro por nível, copiar).
- [x] Erros humanizados (`humanize.rs`).
- [x] Primeiro uso: o core é baixado no primeiro Conectar, com barra de progresso e SHA-256 verificado.
- [ ] Preferências: porta local, DNS, canal do core, iniciar minimizado, autostart.
**Critério:** uma pessoa leiga cola um link e conecta em < 30 s sem ler nada.

### Fase 3 — Assinaturas e conforto (2 semanas)
- [ ] Grupos = assinaturas com auto-update, barra de tráfego/expiração quando houver `userinfo`.
- [ ] "Testar todos" + "Auto (melhor)".
- [ ] Tray (ksni) + notificações + autostart (portal Background).
- [ ] QR pela tela (portal Screenshot + rqrr).
- [ ] Protocolos extras: WireGuard, Hysteria2, SOCKS/HTTP upstream.
- [ ] i18n completa (pt-BR, en), `AdwShortcutsDialog`.

### Fase 4 — TUN e Avançado (2 semanas)
- [ ] Seletor "Túnel (todo o tráfego)" + botão "Conceder permissão" (pkexec setcap + `.policy`).
- [ ] Reaplicar capabilities após update do core; detecção de `nosuid`.
- [ ] Seção Avançado: editor de regras de rota (lista de domínios/IPs → destino), editor JSON bruto com validação `-test`, portas, hotkeys.
- [ ] Protótipo do `obscure-helper` D-Bus para Flatpak.

### Fase 5 — Distribuição (1 semana)
- [ ] Ícone final (128 px full-color + simbólico), screenshots, metainfo completo (releases, branding color).
- [ ] Flathub (modo proxy) — verificar com `flatpak-builder-lint`.
- [ ] AUR: `obscure-desktop` (PKGBUILD com cargo + meson, `options=(!lto)`).
- [ ] Release GitHub com tarball vendorizado.

---

## 6. Riscos e mitigação

| Risco | Mitigação |
|---|---|
| Xray muda formato de share link/config | Parser tolerante + testes com fixtures; canal estável por padrão |
| GNOME sem tray | Tray opcional; estado visível na janela; notificações |
| Flatpak sem TUN | TUN só nativo até o helper existir; texto claro na UI |
| Painéis devolvendo YAML Clash por UA | UA reconhecível + fallback: detectar YAML e avisar |
| Proxy de sistema "preso" após crash | `state.json` + restauração no próximo start |
| Licença GPL do binário Xray (sing dep) | Subprocesso baixado em runtime com hash; aviso em Sobre |

---

## 7. Referências
- Xray-core: https://github.com/XTLS/Xray-core · docs https://xtls.github.io/en/config/ · TUN https://xtls.github.io/en/config/inbounds/tun.html · share link https://github.com/XTLS/Xray-core/discussions/716 · assinatura https://github.com/XTLS/Xray-core/discussions/4877
- Geo: https://github.com/Loyalsoldier/v2ray-rules-dat
- gtk4-rs https://gtk-rs.org/gtk4-rs/stable/latest/book/ · libadwaita NEWS https://gitlab.gnome.org/GNOME/libadwaita/-/blob/main/NEWS · libadwaita 1.9 https://nyaa.place/blog/libadwaita-1-9/
- Template: https://gitlab.gnome.org/World/Rust/gtk-rust-template · Flatpak cargo: https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo · CI: https://github.com/flatpak/flatpak-github-actions
- HIG: https://developer.gnome.org/hig/ · Workbench: https://github.com/workbenchdev/Workbench
- Apps de referência: Snapshot, Loupe, Fractal, Authenticator (GNOME World)
- Concorrentes: Throne https://github.com/throneproj/Throne · oxidom https://github.com/keepinfov/oxidom · v2ray-rs https://github.com/victorzhuk/v2ray-rs · sysproxy-rs https://github.com/clash-verge-rev/sysproxy-rs

---

## 8. Registro de progresso e pendências

### Fase 1 + fatia da Fase 2 (2026-09-20)
Feito: `obscure-core` (links, profile, config, core_manager, supervisor, sysproxy, paths), teste de
integração com Xray real, UI funcional (importar, conectar/desconectar/cancelar, modo de aplicação,
lista de servidores com seleção e remoção confirmada, download do core com progresso, recuperação
de proxy após crash, encerramento limpo). Strings em inglês com 28 traduções em `po/`.

Pendências:
- `stats.rs` (gRPC StatsService), `subscription.rs`, `latency.rs`; reinício com backoff no supervisor.
- UI: sidebar/grupos, detalhe do servidor (QR, copiar link), painel de log (o `ConnectionManager` já
  guarda as últimas 500 linhas), preset de rota, Preferências reais (porta, DNS, canal do core).
- Handler de URL scheme e importação por arquivo.
- Flatpak: proxy do sistema via GSettings precisa de acesso ao dconf (args já no manifest); o
  `kwriteconfig6` do KDE não existe dentro do sandbox — usar `flatpak-spawn --host` (Fase 5).
- Traduções: quando surgirem strings novas, rodar `meson compile -C build obscure-update-po` e
  traduzir à mão nos 28 `.po` (o gerador usado em 2026-09-20 ficou fora do repositório; os `.po`
  são a fonte canônica).
- Rodar sem instalar: no perfil development o binário usa `build/po/` como LOCALEDIR.


### Fase 0 (2026-09-19)
Feito: repositório, workspace com 3 crates, meson + Blueprint, janela libadwaita com menu
(Preferências, Atalhos, Sobre), gschema, `.desktop`, metainfo, ícones provisórios, i18n
(pt_BR base + en), manifest Flatpak Devel (GNOME 50), `cargo-sources.json`, CI, hook pre-commit.

Pendências e observações para as próximas fases:
- **Build Flatpak local não executado**: `flatpak-builder` não está instalado no sistema e nenhum
  runtime GNOME está baixado. O manifest foi validado com `flatpak-builder-lint` (via
  `org.flatpak.Builder`, instalado em nível de usuário). O build completo roda na CI.
- O manifest inclui um módulo `blueprint-compiler` v0.22.2 (tag + commit) por segurança; se o SDK
  do GNOME 50 já trouxer uma versão suficiente, remover o módulo.
- `flatpak-builder-lint manifest` só reclama de `appid-url-not-reachable`: o Flathub espera que o
  app ID `io.github.talesam.Obscure` corresponda ao repositório `github.com/talesam/Obscure`
  (o repo ainda não está publicado, e o nome atual é `Obscure-Desktop`). **Decidir antes do
  Flathub (fase 5)**: renomear o repositório para `talesam/Obscure` ou mudar o app ID para
  `io.github.talesam.Obscure_Desktop`. O linter também avisa que GNOME 51 já existe no Flathub, mas a
  imagem `flatpak-github-actions:gnome-51` ainda não foi publicada (HTTP 404 em 2026-09-19), então o
  manifest segue em GNOME 50 conforme o plano.
- O `<summary>` do metainfo foi trocado para "Conecte-se ao seu proxy com um botão" porque o
  AppStream não aceita ponto final no resumo; o slogan continua na tela principal e no Sobre.
- `rust-version` do workspace ficou em 1.92 (mínimo exigido por `gtk4-sys 0.11.4`).
- `config.rs` é gerado pelo meson (`CODEGEN_BUILD_DIR`) e, sem meson, o `build.rs` gera padrões de
  desenvolvimento; assim `cargo test`/`cargo clippy` funcionam sem `meson setup`.
- O binário roda sem instalar só no perfil `development` (fallback para `build/data`). No perfil
  padrão é preciso `meson install`.
- Os ícones são provisórios (escudo azul); trocar na Fase 5.
- Textos da UI ainda são poucos; `po/en.po` cobre todos os atuais. Ao adicionar strings, rodar
  `meson compile -C build obscure-pot` e `meson compile -C build obscure-update-po`.
- Callbacks de template no Blueprint que recebem `&self` do widget precisam de `swapped`
  (`clicked => $on_x() swapped;`); sem isso o primeiro argumento é o botão e o app aborta.
- Cada `.blp` é um `custom_target` próprio em `data/resources/meson.build` (saída em diretório não
  era rastreada pelo ninja e o gresource ficava desatualizado).
- CI Rust roda em contêiner `archlinux:base-devel` porque o Ubuntu LTS não tem GTK 4.22 /
  libadwaita 1.9.
