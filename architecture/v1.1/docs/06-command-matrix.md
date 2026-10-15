# 06. Command coverage matrix (upstream `egui` → Remote API v1)

**Source of truth:** [`src-gui/src/bridge.rs`](https://github.com/Zaknin/ShockingVRC-Term/blob/egui/src-gui/src/bridge.rs), enum `Command`, SHA `e076bc2ede4726a976c60613031f48c0bc31e0b9`. **Status: Proposed mapping, not implemented.**

Сетевые `op` ниже — **проектные имена**, а не уже существующие Rust функции. Аргументы JSON валидируются адаптером и переводятся в соответствующий вариант `Command`. Запрещено менять логику внутри `Worker::apply` ради нового API.

| Wire `op` | Upstream `Command` | JSON аргументы | Экспозиция |
|---|---|---|---|
| `devices.switch` | `SwitchDevice(usize)` | `index` | remote |
| `zones.add` | `AddZone(Channel, ZoneId)` | `channel`, `zone_id:{zone_type,name}` | remote |
| `zones.add_all_from_avatar` | `AddAllAvatarZones(Channel)` | `channel` | remote |
| `zones.remove` | `RemoveZone(Channel,usize)` | `channel`,`index` | remote |
| `zones.set_mode` | `SetZoneMode(Channel,usize,ContactMode)` | `channel`,`index`,`mode` | remote |
| `zones.set_scale` | `SetZoneScale(Channel,usize,u8)` | `channel`,`index`,`value` | remote |
| `zones.set_threshold_min` | `SetZoneThresholdMin(...)` | `channel`,`index`,`value` | remote |
| `zones.set_threshold_max` | `SetZoneThresholdMax(...)` | `channel`,`index`,`value` | remote |
| `channels.set_frequency` | `SetFrequency(Channel,usize,u8)` | `channel`,`segment`,`value` | remote |
| `channels.set_intensity` | `SetIntensity(Channel,usize,u8)` | `channel`,`segment`,`value` | remote |
| `channels.set_limit_min` | `SetLimitMin(Channel,u8)` | `channel`,`value` | remote |
| `channels.set_limit_max` | `SetLimitMax(Channel,u8)` | `channel`,`value` | remote |
| `channels.set_aggregation` | `SetAggregation(Channel,AggregationMode)` | `channel`,`mode` | remote |
| `config.save` | `SaveConfig` | `{}` | remote |
| `config.load` | `LoadConfig` | `{}` | remote |
| `tuning.set_ukf` | `SetUkf(UkfConfig)` | `config` | remote |
| `tuning.reset_ukf` | `ResetUkf` | `{}` | remote |
| `tuning.set_norms` | `SetNorms(MotionNorms)` | `config` | remote |
| `tuning.reset_norms` | `ResetNorms` | `{}` | remote |
| `modulation.apply` | `ApplyModulation(Channel,ModKind,usize,Box<ModulationConfig>)` | `channel`,`kind`,`segment`,`config` | remote |
| `modulation.clear_segment` | `ClearModulation(Channel,ModKind,usize)` | `channel`,`kind`,`segment` | remote |
| `modulation.clear_channel` | `ClearChannelModulation(Channel)` | `channel` | remote |
| `presets.refresh` | `RefreshPresets` | `{}` | remote |
| `presets.apply` | `ApplyPreset(Channel,usize)` | `channel`,`index` | remote |
| `presets.save_user` | `SaveUserPreset{channel,name,author}` | `channel`,`name`,`author` | remote |
| `presets.delete_user` | `DeleteUserPreset(usize)` | `index` | remote |
| `zone_presets.reload` | `ReloadZonePresets` | `{}` | remote |
| `zone_presets.apply` | `ApplyZonePreset(usize)` | `index` | remote |
| `zone_presets.save` | `SaveZonePreset(String)` | `name` | remote |
| `zone_presets.delete` | `DeleteZonePreset(usize)` | `index` | remote |
| `alarm.set_config` | `SetAlarmConfig(Box<AlarmConfig>)` | `config` | remote |
| `alarm.set_channels` | `SetAlarmChannels(AlarmChannels)` | `channels` | remote |
| `alarm.test` | `AlarmTest` | `{}` | remote |
| `alarm.stop` | `AlarmStop` | `{}` | remote |
| `alarm.snooze` | `AlarmSnooze` | `{}` | remote |
| `prefs.set_auto_save` | `SetAutoSave(bool)` | `enabled` | remote |
| `prefs.set_alarm_tab_visible` | `SetAlarmTabVisible(bool)` | `enabled` | remote |
| — | `CheckUpdate` | — | host-local only |
| — | `StartUpdateDownload` | — | host-local only |
| — | `SkipUpdateVersion` | — | host-local only |
| — | `DismissUpdate` | — | host-local only |

**Общее**: `channel` = `A|B`; `segment` = 0,1,2,3; `kind` = `freq|intensity`.
**Remote permission note (Option A ACCEPTED):** Host-only `Remote Maximum Strength` проверяет перед enqueue **ТОЛЬКО** `channels.set_limit_min` и `channels.set_limit_max` **и лишь при `enabled=true`**. По умолчанию (`enabled=false`) Remote Terminal и local `egui` меняют Power Limits по обычным правилам `0..200`; ранее сохранённое число cap игнорируется. Запрещена запрошенная запись > **cap соответствующего канала** (два независимых A/B: `max_by_channel.A/B`, ADR-021); по всем остальным операциям `zones.*`, `presets.*`, `modulation.*`, `config.load`, `alarm.*`, `channels.set_intensity` и т. д. дополнительных cap-based ограничений **нет**. Это не гарантия физического output <= cap; реальный Host state всегда отображается. При `min` Host > cap **этого канала** команда `set_limit_max` может быть нормализована upstream выше запрошенного значения — такую ситуацию нужно отклонять без ложного ACK, не меняя core. См. [`11-remote-session-and-strength-cap.md`](11-remote-session-and-strength-cap.md). Для прочих enum (`ContactMode`, `AggregationMode`, `AlarmChannels`, `OldZoneType`, `ModulationSource/Function`) wire значения надо *явно перечислить* по исходникам перед написанием DTO/JSON Schema; не угадывать порядок/числа и не применять `.to_string()` как сериализацию контракта.

**Массивы и индексы**: `zones.*`, `presets.apply`, `presets.delete_user`, `zone_presets.*` используют индексы в исходном Worker. Клиенту передаются полные списки и порядок, `index` соответствует **текущему** списку. Изменение списков между Snapshot/каталогом и применением может привести к другой цели; `Bridge` пока не предоставляет атомарную проверку версии + выполнение. API может отвергнуть явно устаревшие запросы при предварительной проверке, но **не гарантировать атомарность**, пока не принят отдельный ADR на Worker.

**Состояние (read models)**: `Snapshot` покрывает `CliConfig`, `CliStatus`, `AlarmConfig/AlarmStatus`, `avatar_zones`, `vrchat_found`, `osc_port`, `oscquery_port`, `devices`, `active_device`, `revision`. Отдельные `Shared` — `presets`, `zone_presets`, `toast`, `update`. Для Remote API экспортируем presets/zone_presets/тосты и buffered logs; update state (загрузка executable) локален, исключён. Настройки visual theme/animated backdrop также локальны.

**Тесты coverage**: для каждой `remote` строки table-driven unit test (`op` → нужный `Command`) и отрицательные значения аргументов, plus интеграционные тесты на `Bridge::detached()` с inspection receiver и `BridgeRemoteBackend`. Пользовательскую логику не тестировать реальными воздействиями; использовать fixtures/mock.
