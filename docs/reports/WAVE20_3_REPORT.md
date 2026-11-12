# Волна 20.3 — довести компенсацию restore до фактической транзакционности

## Уточнение по волне 20.2
Замечания A2 относились к первому архиву `openscale-code-wave202.zip`, собранному ошибочно (из копии wave201 вместо рабочего дерева). Рабочее дерево на коммите `8e30880` уже содержало: `unquarantine_sidecars`, составные ошибки quarantine, проверку pre до удаления sidecar-ов в rollback, компенсации prepare/rename/verify. Тем не менее в 20.2 не хватало тестируемости этих компенсаций — закрыто в 20.3.

## Новый порядок restore (restore_core, app/openscale/src/backup.rs)
1. Эксклюзивная блокировка обслуживания; проверки снимка (integrity, schema).
2. `save_current_state` — снимок исходного состояния (pre). Ошибка — отмена, ничего не изменено.
3. `quarantine_sidecars` — изоляция старых WAL/SHM; внутренняя компенсация возвращает уже изолированные, ошибки возврата входят в составную ошибку.
4. Подготовка tmp и копирование снимка. Ошибка → `unquarantine_sidecars` + составная ошибка. pre не удаляется.
5. Атомарная замена main. Ошибка → та же компенсация (замены не было).
6. Проверка восстановленной базы. Ошибка → `rollback_to_saved` (pre проверяется ДО удаления рабочих sidecar-ов; ошибка отката — в составном сообщении).

После этапа 3 нет ни одного `?` или раннего return без компенсации: путь prepare (remove tmp/tmp_wal, copy), путь rename и путь verify обёрнуты в compose_with_compensation или rollback_to_saved.

## Инъекция
`RestoreOps { save, copy, rename_main, verify }` (type-алиасы `SaveOp`/`RestoreOp`/`VerifyOp`): production-набор из реальных `save_current_state`, `copy_fsync`, `fs::rename`, `verify_restored`; `restore_backup` — тонкая обёртка над `restore_core(..., &RestoreOps::production())`. Публичные интерфейсы не изменены.

## Результаты шести обязательных сценариев (backup::tests, 40 passed)
1. `restore_copy_failure_returns_sidecars_byte_for_byte` — отказ copy после изоляции: `-wal`/`-shm` возвращены побайтно (`WAL-ORIG`/`SHM-ORIG`), main побайтно исходный, pre существует.
2. `restore_rename_failure_returns_sidecars_byte_for_byte` — отказ rename: sidecar-ы возвращены побайтно, main побайтно исходный.
3. `restore_verify_failure_rolls_back_full_state` — отказ verify: полный откат (main побайтно исходный), исходный `-wal` сохранён в изоляторе побайтно; успешный откат переименовывает pre в main (проверено комментарием в тесте).
4. `restore_compensation_failure_reports_both_causes_and_paths` — отказ copy + отказ возврата: ошибка содержит обе причины («копирование снимка не удалось», «компенсация не удалась», «не вернуть изолированный»), путь изолятора (`*.db-wal.stale`) и рабочий путь (`kiosk.db-wal`); изолированный файл остался в изоляторе побайтно; main не тронут.
5. `rollback_missing_pre_keeps_files_byte_for_byte` — отсутствующий pre: main, `-wal`, `-shm` побайтно неизменны.
6. `restore_success_ignores_stale_sidecars` — успешный restore: посторонние sidecar-ы не применены к новой базе (отсутствуют на рабочих путях), сохранены в изоляторе побайтно, снимок применён (plu_count из снимка).

Тесты отказа компенсации (отдельно): сценарий 4 выше + юниты `unquarantine_compensation_failure_is_reported` и `unquarantine_returns_quarantined_sidecars`. Интеграционные тесты Windows-путей (каталоги-помехи, held-соединение) сохранены: `restore_prepare_failure_returns_sidecars_and_keeps_db`, `restore_copy_failure_keeps_original_db`, `restore_rename_failure_keeps_original_db`, `restore_fails_when_wal_quarantine_blocked` (переписанный в 20.2, оставлен).

## Проверки
- `cargo test --workspace --target x86_64-pc-windows-msvc` — 270 passed, 0 failed (backup::tests: 40).
- `rustfmt --check --edition 2024 app/openscale/src/backup.rs` — ok.
- `cargo clippy --workspace --all-targets` — 0 ошибок; новые предупреждения в backup.rs отсутствуют (type_complexity устранён алиасами), старые не тронуты.
- `cargo zigbuild --release` (bin + lib/tests) — ok.
- Деплой на стенд: сервис заменён, active, журнал чист (0 panic/ERROR); ui_scenarios 1 passed.
- Физический restore на стенде: `--backup` → `--restore` снимка → «восстановлено», `--db-check`: integrity ok, товары на месте.

## Ограничения
- При успешном откате pre переименовывается в main — отдельный файл pre исчезает; это поведение `rollback_to_saved` (файл становится main), тесты учитывают.
- Старые предупреждения clippy в `datastore` и `setup` — вне задачи.

## Артефакты
- Коммит: волна 20.3.
- Стенд: сервис active, свежий снимок и pre-restore в /home/scales/backups.
