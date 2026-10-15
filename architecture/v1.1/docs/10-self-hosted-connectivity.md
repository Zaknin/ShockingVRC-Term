# 10. Self-hosted connectivity — BYO signaling / STUN / TURN

**Status: BYO/defaults decisions ACCEPTED · implementation & deployment mechanics DRAFT · 2026-10-15 · v0.4**

## Утверждённый контракт по умолчанию

- ShockingVRC-Term **не предоставляет общего signaling, STUN, TURN**; URLs **не подставляются** из кода или удалённого каталога при пустых настройках.
- Settings `rendezvous_url = null`, `stun_urls = []`, `turn_servers = []`, `turn_credentials = null`. На первом запуске нет запроса к какому-либо стороннему rendezvous/STUN/TURN.
- Direct LAN, VPN, public IP, DDNS + HTTPS/WSS работают независимо от пользовательской ICE инфраструктуры.
- Натуральный статус `not_configured` для traversal; не показывать его как техническую неисправность, если пользователь ещё не включал этот режим.
- Каждый Host/Terminal настраивается собственными адресами. Клиент может хранить несколько разных Host profiles; выбор профиля не меняет настройки другого Host. Не отправлять Host login/password в signaling или TURN.

Файл `schemas/connectivity-defaults.example.json` содержит только значения по умолчанию **для публичных инфраструктурных конечных точек**, а не полный `remote_access.json`: не смешивать с Host login, портом слушателя или локальным хранением ключей.

## Развёртывание для каждого пользователя

Reference distribution (проектное предложение):

1. `shockingvrc-rendezvous`: self-hostable public HTTPS/WSS signaling, exchange SDP/ICE, host presence/pairing, TTL sessions, auth to signaling. Не контролирует устройства, не принимает Remote API login/password, не содержит device engine.
2. `coturn` или совместимый STUN/TURN: публичный IP и нужные UDP/TCP/TLS interfaces, отдельный TURN realm/конфигурация. Relay передаёт encrypted WebRTC traffic, не декодируя его.
3. Система выдачи TTL TURN credentials: предлагается own rendezvous + TURN REST shared secret **только на сервере**; TURN static account как совместимость при явной настройке, решение ещё требует ADR.
4. Host: URL signaling, STUN URLs, TURN URLs, trusted signaling/TLS trust, включение traversal. Terminal: URL signaling для выбранного Host profile, Host identity pin, собственная аутентификация на Host.
5. Deployment documentation: потребности по портам/Firewall, HTTPS certificate, DNS, coturn relay range, NAT/firewall diagnostics, обновление/бэкапы, удаление credential из логов.

Для успешного ICE P2P через LAN STUN/TURN может быть не нужен (host candidates), **но нужен обмен ICE описаниями через выбранный signaling**. За разными NAT на практике обычно нужен STUN, а при невозможности прямого UDP — TURN relay. Сервер STUN/TURN не создаёт TCP port forwarding для direct WSS.

## Частный пример: московская машина с OpenShock

Владелец проекта предложил разместить **отдельные** `shockingvrc-rendezvous` и `coturn` на существующем московском сервере, где работает OpenShock. Это **опциональное частное deployment решение**, не hard-coded production default и не обещание публичного общего сервиса. Перед реальным размещением отдельно проверить:

- Реальные OS, CPU/RAM, публичную IP-достижимость, пропускную способность и использование портов на московском сервере.
- Отсутствие пересечения TCP/UDP портов и firewall policy с работающим OpenShock.
- Адреса DNS и TLS-сертификаты для signaling; TURN public relay address, UDP/TCP port и relay port range.
- Токены и TURN secret хранить отдельно от OpenShock и конфигурации ShockingVRC Host.
- Разделение сервисов (systemd или Docker), журналов и обновлений; не перезапускать OpenShock ради ShockingVRC.

Не утверждать, что московские сервисы развернуты, пока не получены реальные host/network данные и результаты запуска.

## Публичный API настроек (проектный эскиз)

Host Remote Access settings: `traversal.enabled`, `rendezvous_url`, `stun_urls`, `turn_servers`, `signaling_tls_trust`, `active_connections`, `last_error`.

Remote Terminal per-host profile: `direct_url`, `trusted_host_identity`, `rendezvous_url`, `stun_urls`, `turn_servers`, `prefer_direct`, `allow_relay`, `connection_path`.

Credentials TURN допускаются только в OS credential store либо получаются в session-scoped ephemeral form; не помещать пароль Host внутрь TURN credentials, URL query или logging. Имена полей — draft; wire/API format согласуется отдельно.

## Проверки

1. Чистая установка: ни один предустановленный STUN/rendezvous/TURN сервер не опрашивается (сетевой trace).
2. Настроенный чужой сервер A: подключение к нему, а не к нашему московскому B; отсутствие hidden fallback.
3. Различные профили A/B: профили и Host identity не смешиваются.
4. Только signaling: попытка ICE host-candidate, без принудительной отправки на STUN/TURN.
5. TURN relay: два NAT, direct ICE failed, используется явно указанный TURN; read-back API.
6. Московская private deployment: раздельные сервисы, незатронутый OpenShock, no hard-coded endpoints в build artifacts.

**Scope note:** создание VPS и публикация deployment пакетов отдельно согласуются; данная спецификация не меняет алгоритмы устройства.
