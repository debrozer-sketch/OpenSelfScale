# Волна 20.2 — транзакция restore с компенсациями

## Задача
Завершить атомарность восстановления БД (`app/openscale/src/backup.rs`): участок от изоляции sidecar до проверки восстановленной БД — одна логическая транзакция с компенсациями. Без рефакторинга и без изменения архитектуры бэкапа.

## Последовательность restore

**До волны 20.2:**
1. Блокировка обслуживания, замер точки восстановления.
2. `save_current_state` — сохранение исходного состояния (pre).
3. `quarantine_sidecars` — изоляция старых -wal/-shm.
4. Подготовка tmp-файлов: удаление остатков `restore.tmp.*` через `let _ =` (ошибки проглатывались), копирование снимка.
5. Переименование tmp → main; при ошибке — только лог, изолированные sidecar-ы оставались изолированными (исходная база без WAL-состояния).
6. Проверка восстановленной БД; при ошибке — `rollback_to_saved`, при этом pre удалялся до проверки отсутствия, рабочие sidecar-ы могли удаляться до обнаружения отсутствия pre.
7. Ошибки компенсаций (возврат изолированных sidecar-ов) только в лог — пользователь не видел потери.

**После волны 20.2:**
1. Блокировка обслуживания, замер точки восстановления.
2. `save_current_state` — без изменений.
3. `quarantine_sidecars` — ошибка компенсационного возврата теперь входит в составную ошибку (не только лог).
4. Подготовка tmp и копирование снимка: удаление остатков и копирование проверяемые; при ошибке — `unquarantine_sidecars` (возврат изолированных sidecar-ов), исходная база вновь полна. pre не удаляется.
5. Атомарная замена main; при ошибке — та же компенсация (замена атомарна: не удалась — main не тронут).
6. Проверка восстановленной БД; при ошибке — `rollback_to_saved`: проверка наличия pre ПЕРЕД удалением рабочих sidecar-ов, полный откат; ошибка отката — в составном сообщении.
7. Все ошибки компенсаций возвращаются вызывающему вместе с причиной основного этапа (`compose_with_compensation`).

## Изменения
- `app/openscale/src/backup.rs`:
  - `quarantine_sidecars` — ошибки возврата уже изолированного добавляются в составную ошибку;
  - новый `unquarantine_sidecars` — возврат изолированных sidecar-ов, все ошибки наружу;
  - новый `compose_with_compensation` — составное сообщение «причина; компенсация не удалась: …» + лог::error;
  - `rollback_to_saved` — проверка `!pre.exists()` перенесена до удаления рабочих sidecar-ов;
  - `restore_backup` — переписан как транзакция (5 этапов), doc-комментарий с последовательностью;
  - тесты: `restore_fails_when_wal_quarantine_blocked` переписан (см. «Находки»), новые `restore_prepare_failure_returns_sidecars_and_keeps_db`, `restore_copy_failure_keeps_original_db`, `restore_rename_failure_keeps_original_db`, `unquarantine_returns_quarantined_sidecars`, `unquarantine_compensation_failure_is_reported`, `rollback_missing_pre_keeps_work_sidecars`; `rollback_reports_sidecar_removal_error` уточнён.

## Находки
- Тест волны 20.1 `restore_fails_when_wal_quarantine_blocked` проверял не то, что заявлял: `quarantine_sidecars` на нём не падал (held-соединение читает — -wal не создаётся, изолировать нечего), restore падал на rename (held держит main), а assert `err.contains("-wal")` проходил случайно — из-за тега каталога `setup("sidecar-wal")`. Диагностика показала: каталог-помеха изолятора попадал в kiosk.db-wal через `unquarantine_sidecars`, который принимал его за «изолированный sidecar». Исправлено: held с активной транзакцией (INSERT) реально создаёт -wal, изолятор блокируется → quarantine падает на изоляторе.
- Windows-ограничение: sidecar-ы от живого соединения не переименовываются (os error 32), от мёртвого — удаляются sqlite, поэтому интеграционно «успешная изоляция + отказ prepare» невоспроизводим; возврат sidecar-ов покрыт юнит-тестами `unquarantine_*`, отмена — интеграционным `restore_prepare_failure_*`.

## Проверки
- `cargo test --workspace --target x86_64-pc-windows-msvc` — 265 passed, 0 failed (backup::tests: 35).
- `rustfmt --check --edition 2024 app/openscale/src/backup.rs` — ok.
- `cargo clippy --workspace --all-targets` — 0 ошибок; в backup.rs остались только старые предупреждения (setup, вне объёма волны).
- `cargo zigbuild --release -p openscale --bin openscale --target x86_64-unknown-linux-gnu.2.41` — ok; тестовый бинарник собран.
- Деплой на стенд: сервис заменён, `openscale-kiosk` active, журнал чист (0 panic/ERROR).
- Тестовый бинарник на стенде: `ui_tests::ui_scenarios` — 1 passed.
- Физическая проверка restore на стенде: `--backup` (снимок создан) → остановка киоска → `--restore` снимка → «восстановлено», `--db-check`: integrity ok, товары на месте. Киоск запущен.
- Компенсации, проверенные тестами: отказ prepare (каталог на tmp_wal), отказ копирования (каталог на tmp), отказ rename (открытый main), отказ компенсационного возврата (юнит, каталог на рабочем пути), полный откат при ошибке проверки (волна 20.1), отсутствие pre при откате.

## Ограничения
- Отказ копирования снимка после successful prepare при наличии реально изолированных sidecar-ов интеграционно не воспроизводим на Windows (см. «Находки»); покрыт комбинацией интеграционного теста отмены и юнит-тестов возврата.
- Предупреждения clippy в `datastore` и `setup` в backup.rs — вне объёма волны, не тронуты.

## Артефакты
- Коммит: волна 20.2 (см. git log).
- Стенд: БД /home/scales/plu.db, сервис active, свежий снимок в /home/scales/backups.
