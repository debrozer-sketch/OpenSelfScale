# Волна 20: корректирующая волна по результатам ревью wave19

Ревью архива wave19 нашло дефекты: часть гарантий волн 17–19 не сохранена.
Волна 20 исправляет их по группам A (fail-closed БД и откаты), B (конкуренция
синхронизаций), C (достоверность статусов принтера), D (мелкие дефекты).

## A. Fail-closed БД и атомарность восстановления

### A1. Пустая БД создавалась при чтении статуса синхронизации
**Было:** `sync_status` (UI-экран «Обслуживание», панель синхронизации)
открывал БД через `datastore::open` — на потерянной/пустой базе путь
создавал новый файл с полной схемой, и integrity_check пустого файла
(`ok` для пустого SQLite) маскировал регрессию волны 17: fail-closed обходился
через экран статуса.

**Стало:**
- `backup::probe_startup` сначала проверяет `schema_ok` (наличие таблиц
  `plucodes`, `print_log`, `app_settings` в `sqlite_master`) до любых миграций.
- Новый `backup::open_existing(db_path) -> ExistingDb`: открывает только
  существующий файл (SQLite READ_WRITE | URI без CREATE), берёт
  MaintenanceLock (shared), проверяет таблицы. Файл не создаётся никогда.
- `sync_status::begin/finish` переведены на `open_existing`.

**Файлы:** `app/openscale/src/backup.rs`, `app/openscale/src/sync_status.rs`.

**Тесты:** `probe_rejects_empty_sqlite_file_with_marker`,
`probe_rejects_partial_schema_with_marker`,
`probe_first_install_without_marker_still_works`,
`probe_accepts_full_db_with_marker`,
`sync_status_open_does_not_create_missing_db` — потерянная БД не воссоздаётся,
пустая база с маркером не мигрируется.

**Проверено:** программно (unit-тесты). Физически: `--db-check` на стенде после
деплоя — integrity ok, 218 товаров.

### A2. Откат восстановления не покрывал WAL/SHM
**Было:** `restore_backup` при неудаче проверки снимка откатывал только
main-файл базы; ошибки `pre.db-wal`/`pre.db-shm` глотались (`let _ =`).
При WAL-режиме откат мог оставить базу в несогласованном состоянии, а
молчаливые ошибки — скрыть повреждение.

**Стало:** `save_current_state()` (Backup API, fallback на файловую копию
main+wal+shm; ошибка копирования sidecar отменяет замену) и
`rollback_to_saved()` (все шаги с ошибками наружу: удаление чужих sidecar,
rename pre → db_path, возврат pre-sidecar). Полный откат при любой неудаче
проверки снимка.

**Файлы:** `app/openscale/src/backup.rs`.

**Тесты:** `restore_rollback_restores_wal_state_fully` — триада main+wal+shm
заморожена, урезанный снимок отброшен, sidecar-файлы проверены до открытия БД;
`restore_refuses_to_start_when_pre_save_fails` — pre-путь занят каталогом,
восстановление отказывается начинать, текущее состояние не тронуто.

## B. Конкуренция синхронизаций

### B1. FTP-приём без sync.lock
**Было:** `sync::run()` не брал sync.lock — параллельный запуск (демон-таймер,
drop-выгрузка, CLI) мог одновременно писать в каталог приёма и БД.

**Стало:** `run()` создаёт каталог приёма и берёт `sync.lock` (тот же, что у
drop и CLI) до `begin()`; занятая блокировка — выход с ошибкой до любых
изменений состояния.

**Файлы:** `app/openscale/src/sync.rs`.
**Тесты:** `sync_lock_is_exclusive` остаётся зелёным; поведение включено в
сценарии S19.

### B2. finish мог перезаписать чужую/завершённую попытку
**Было:** «занятая блокировка» писалась в статус как ошибка; параллельная
попытка могла перезаписать статус другой.

**Стало:** идентификатор попытки (`id`, now_nanos) хранится в записи и в
процессном `ATTEMPT`-мьютексе. `begin` пишет `run`-запись и сохраняет id;
`finish` молчит без своего id (занятая блокировка не является ошибкой) и
пишет только если запись ещё `run` с тем же id. Ошибка не затирает
последний успех (`u` переносится).

**Файлы:** `app/openscale/src/sync_status.rs`.
**Тесты:** `foreign_finish_cannot_override_current_attempt`,
`stale_finish_after_new_attempt_does_not_overwrite`,
`busy_lock_refusal_is_not_recorded_as_error` (честный production-finish).

## C. Достоверность статусов принтера

### C1/C2. Молчание принтера и биты бумаги
**Было:** `Monitor::feed(None)` ставил `seen=true` (принтер «отвечал» без
ответа); `paper_end`/`paper_near_end` не вычислялись из L4.

**Стало:** `seen=true` только при реальном ответе; при молчании
`SILENT_CONFIRM` подряд — `fault`+`silent` («ошибка связи»), статусы бумаги
сбрасываются как недостоверные. Биты: paper_end = 0x40, near_end = 0x08
(при валидном L4 без fault-бита), норма L4_OK=0x36, лоток — биты 0x60.

**Файлы:** `app/openscale/src/print.rs` (`Monitor::feed`, `PrinterHealth`).
**Тесты:** `feed_none_does_not_mark_seen`, `paper_states_come_from_l4_bits`
(0x3E near-end, 0x76 paper_end), `silence_is_link_error_and_clears_paper_status`.

### C3. Единый статус с экраном «Состояние»
`print::status_line(printer_enabled) -> (String, i32)` — единственный источник
текста статуса принтера (выключен в настройках / ошибка связи / авария / нет
ответа / кончилась бумага / занят: этикетка в лотке / бумага заканчивается /
готов). «Готов» — только при подтверждённом ответе. Экраны «Обслуживание» и
«Состояние» читают его.

**Файлы:** `app/openscale/src/print.rs`, `app/openscale/src/settings.rs`.
**Тесты:** `status_line_respects_enabled_seen_and_fault`, сценарий S18.

## D. Мелкие дефекты

- **D1.** Сборка статусов «Обслуживания» (integrity_check, чтение БД) вынесена
  из UI-потока: после резервного копирования и после тестовой печати строки
  собираются в фоновом потоке, invoke получает готовые rows
  (`settings.rs`, два вызова в `invoke_from_event_loop`).
- **D2.** Округление суммы теперь снимок покупки: колонка `print_log.rounding`
  (+ миграция ALTER), поле `PrintLogRow.rounding`, запись при продаже.
  Повторная печать воспроизводит исходное округление; для старых записей
  (NULL) — текущая настройка (`datastore/src/lib.rs`, `print.rs`).
- **D3.** CLI `--print-test` отказывается работать при активном киоске
  (`backup::kiosk_running`, теперь pub): порт принадлежит демону
  (`main.rs`).
- **D4.** CLI `--backup` берёт MaintenanceLock shared: снимок согласован,
  restore не начнётся во время копирования (`main.rs`).
- **D5.** FTP: connect_timeout + read/write таймауты до login — зависший
  сервер больше не останавливает приём на неограниченное время
  (`suppaftp::connect_timeout`, `sync.rs`).
- **D6.** Лимит изображений применяется при чтении: потоковое `ftp.retr` с
  обрезкой на 4 МБ до накопления в памяти — FTP SIZE больше не обходится
  (`sync.rs::fetch_bytes`).
- **D7.** `datastore::print_log_mark` возвращает `Result`; ошибки маркировки
  журнала печатаются в лог с указанием rowid и последствия (`print.rs`,
  `datastore`).

## E. Проверки

| Проверка | Результат |
|---|---|
| `cargo test --workspace` (Windows) | **249 passed, 0 failed** (55+2+18+162+12) |
| `cargo fmt --check` | форматирование применено к затронутым файлам (rustfmt установлен в этой волне; до этого компонент отсутствовал) |
| `cargo clippy --workspace --all-targets` | 0 ошибок; ~45 предупреждений (в основном в коде ранних волн: deref, Default, range-contains) — не исправлялись, чтобы не расширять волну |
| `cargo zigbuild --release` (linux.2.41) | ok |
| Деплой `push.py --deploy` | ok: sha256, запуск проверен, prev сохранён, сервис active |
| UI-сценарий S1–S19 на стенде | **ok** (`openscale-tests --test ui_tests::ui_scenarios`) |
| Полный lib-набор на стенде | 156 passed, 8 failed — все 8 env-обусловлены: 6 restore-тестов требуют остановленного киоска (на проде активен), 2 `cmd_timeout` зависят от PATH (`sleep` не найден). Полный набор на стенде исторически не гоняется; на Windows все проходят |
| `--db-check` | integrity ok, 218 товаров |
| `--weight` | связь есть («НЕ СТАБИЛЬНО: 0.000» — весов на платформе нет) |
| `--print-test` (киоск остановлен) | печать завершена, выдана=true, трасса L4 306ms 0x1E |
| Журнал после деплоя | 0 panic/ERROR после старта 15:39:06 (UTC) |

## Ограничения

- `kiosk_running`-проверка D3 использует существующий механизм (systemd-статус);
  отдельной flock-блокировки принтерного порта нет — не создавался второй
  независимый механизм.
- clippy-предупреждения старого кода не устранялись (вне объёма волны).
- Каталог меток сессий `F:\Project\ai-session-history-visualizer\catalog`
  недоступен — метка сессии не создана.
- Общий счётчик panic/ERROR в kiosk.log содержит исторические записи прошлых
  сессий (ротация включена); после текущего деплоя новых нет.
