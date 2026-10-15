# 03. Connectivity: прямой HTTPS/WSS, DDNS, UPnP и ICE/TURN

**Status v0.15: BYO network infrastructure accepted; WSS/RTC transport API design approved ADR-028–038 (identity binding conditional ADR-033). Deployment/implementation still pending.**

## Режимы подключения

| Режим | Канал | Требуется домашний порт-маппинг | Инфраструктура |
|---|---|---|---|
| LAN | прямой HTTPS/WSS по private IP | нет | нет |
| VPN | прямой HTTPS/WSS по VPN IP | нет для приложения | VPN уже имеется |
| Public IPv4 / DDNS | прямой HTTPS/WSS по TCP | UPnP/NAT-PMP/PCP или manual | DNS/DDNS обновляет роутер/клиент |
| NAT/CGNAT | WebRTC DataChannel, ICE → TURN fallback | нет | **доступный public signaling/rendezvous; TURN для relay** |

Прямой адрес может быть IP или FQDN/DDNS. Встроенный DDNS updater не обязателен, если роутер пользователя обновляет имя. DDNS не открывает порт. `4318/TCP` — **утверждённый порт по умолчанию** для direct API (ADR-036), изменяемый в настройках Host.

## Direct HTTPS/WSS identity

Принято: direct TLS pinning без обязательного домена. Host устойчиво хранит TLS identity и отображает проверяемый fingerprint/SPKI pin. Terminal перед отправкой логина независимо проверяет его по сохранённому профилю. First trust — сверка с fingerprint Host через независимый канал; автоматический непроверенный TOFU не обещаем. Изменение IP/DDNS не меняет Host identity. Для ICE потребуется привязать ephemeral DTLS fingerprint к **той же доверенной Host identity**; migration/identity-key ADR открыт (см. [`07`](07-ice-turn-signaling.md)).

## UPnP / NAT-PMP / PCP

- Remote Access ON → TLS listener → по выбору пользователем discovery роутера и TCP mapping. По умолчанию автоматическое открытие порта опционально, не скрытая фоновая процедура.
- Проброс действует на `external_tcp_port → local_tcp_host:listen_port`; обновлять lease, удалять свой mapping при OFF/shutdown, насколько это возможно.
- Неуспех mapping не ломает LAN/VPN, Host GUI или BLE/OSC.
- Если mapping нет — понятная инструкция для ручного forwarding. Роутеры пользователей разные, интеграции с FortiGate не предусматриваем. Для входящих TCP connections также нужно разрешающее правило Windows Firewall; приложение показывает проверяемые инструкции или запрашивает разрешение, но успешный UPnP mapping его не гарантирует.
- Статусы `Mapped`, `Mapping failed`, `External reachability unverified`, `Verified externally`. `Mapped` сам по себе **не гарантирует** доступность извне (двойной NAT/CGNAT/firewall/hairpin).

## ICE/STUN/TURN

Входит в архитектурный scope по решению пользователя от 2026-10-15. **Это не расширение UPnP и не открытие TCP-порта через STUN.** Для direct-связи ищем UDP ICE candidate pair; при невозможности используем TURN relay. Один `Remote API` поверх WSS **или** WebRTC reliable ordered DataChannel. Публичный signaling нужен для offer/answer/candidate exchange, TURN — для реального CGNAT fallback. Подробности, Host identity binding, topology и тесты: [`07-ice-turn-signaling.md`](07-ice-turn-signaling.md).

Прямой WSS не зависит от внешней signaling инфраструктуры. Потеря TURN не изменяет локальную работу ShockingVRC. Переключение транспорта требует нового login и полного Snapshot, неподтверждённые команды не переигрываются автоматически.

## Host GUI: настройки сети (предлагается)

```text
Remote Access              OFF / ON (default OFF)
Username / Set Password    локально на Host
Listen TCP port            4318 [approved default ADR-036]
Bind address               0.0.0.0 / selected interface [open UX]
Automatic port mapping     OFF/ON — UPnP, NAT-PMP, PCP
Router mapping state       auto/manual/error, ext port, gateway, lease
Direct endpoint            IP / hostname:port
Server Host fingerprint    Copy/QR for trusted pairing
NAT traversal              OFF/ON [separate milestone]
Rendezvous endpoint        (empty by default; user-supplied URL)
STUN server URLs           [] (empty by default; user-supplied)
TURN server URLs           [] (empty by default; user-supplied)
TURN credentials           unset (user-supplied or issued by OWN rendezvous)
TURN relay status          not configured/reachable/failure
Active remote sessions     count
```

## Сервисы принадлежат пользователю (ACCEPTED)

В поставке **никаких предустановленных адресов signaling, STUN, TURN, публичных STUN DNS и учётных данных TURN**. Значения отсутствуют (`null` / пустой список); приложение **не обращается к какой-либо общей инфраструктуре автоматически**. Каждый пользователь вручную указывает свои сервисы; можно использовать личный VPS, отдельный публичный сервер или сервис выбранного провайдера. Предлагаем подготовить deployable комплект rendezvous + coturn и самостоятельный deployment guide.

Для конкретной частной установки пользователь предложил московский сервер, где работает OpenShock: сервисы ShockingVRC размещаются **отдельно**, без зависимости от OpenShock и без автоматического предоставления московского узла сторонним пользователям. Адреса, порты и сетевую ёмкость сервера надо проверить при развёртывании. TURN credentials не являются Host login/password: лучше временные, выдаваемые **собственным** signaling с помощью TURN REST credentials, но точный механизм остаётся проектным решением.

При пустых настройках NAT traversal обозначен `not_configured`, direct LAN/VPN/WAN остаётся доступен. Подробнее: [`10-self-hosted-connectivity.md`](10-self-hosted-connectivity.md).

## Диагностика

- `UPnP success` не означает `WAN reachable`; внешняя проверка — только клиентом из другой сети.
- STUN result — информация о конкретном UDP NAT binding, не о доступности входящего TCP listener.
- CGNAT/двойной NAT: Direct TCP может не работать, ICE может найти P2P путь, TURN relay покрывает случаи, когда P2P невозможен.
- Контроль пути: direct WSS, ICE host/srflx, TURN relay; UI показывает реальный negotiated path, а не обещание.

RFC: STUN RFC 8489; ICE RFC 8445; TURN RFC 8656; WebRTC Data Channels RFC 8831. Реализация и host identity binding — перед code review.
