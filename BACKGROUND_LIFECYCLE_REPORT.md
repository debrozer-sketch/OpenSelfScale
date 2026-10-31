# Жизненный цикл фоновых операций — OpenScale

Дата: 2026-10-31. Деплой: sha256 `899ccac4503c3836ebcb77c8d4fdb7d3a55c9dd665d367b12404c93ebb097be0`.

## 1. Карта потоков и фоновых операций

| Поток/задача | Где | Остановка | Оценка |
|---|---|---|---|
| `weight-poll` | weight.rs, `WeightSource::spawn` | `Drop`: stop + join, прерываемый сон | ок (исправлено ранее) |
| printer daemon | print.rs, вечный цикл с catch_unwind-перезапуском | не останавливается — by design, ack 90 с, STALE_JOB 25 с | ок |
| watchdog | watchdog.rs, beat + supervisor | abort при молчании UI 25 с — by design | ок |
| net monitor | net.rs, опрос конфигурации раз в 3 с | subprocess с deadline 2 с + kill | ок |
| auto-sync drop/FTP | sync.rs, вечный цикл, catch_unwind | FTP connect/read/write таймауты, повтор при панике | ок |
| thumbs | thumbs.rs, однократный поток, бюджет 300 с | самозавершается | ок |
| печать покупки | tick.rs spawn → канал (op, res) | cancel по PRINT_TIMEOUT 30 с; request ack 90 с | ок |
| pin task / menu tasks | spawn_pin_task, spawn_menu_task | catch_unwind сбрасывает busy-флаги | ок |
| relayout / refresh данных | flow.rs, `RELAYOUT_BUSY` / `REFRESHING` | guard от повторного запуска | ок |
| отчёты | REPORT_GEN (поколение) | устаревший результат пропускается | ок |
| повторная печать | REPRINT_BUSY | одна попытка за раз | ок |
| settime | settings.rs `run_settime` | **было: нет** (исправлено) | D1 |
| VNC toggle | settings.rs spawn | **было: нет + без BUSY** (исправлено) | D2 |
| калибровка принтера | settings.rs spawn | **было: без PRN_BUSY, BUSY зависал при панике** (исправлено) | D3 |
| diag / feed / export | settings.rs сырые spawn | самозавершающиеся с таймаутами | P3, отложено |

Каналы: `print_rx` (тик, 1 producer на покупку, op-фильтр поздних), ack-каналы
print.rs (recv_timeout 60–180 с), sync_rx (1 сообщение на выгрузку) — переполнение
невозможно при текущих producer'ах.

## 2. Найденные дефекты

| ID | Приоритет | Дефект | Доказательство | Фикс |
|---|---|---|---|---|
| D1 | P1 | `run_settime`: `Command::output()` без таймаута; зависший хелпер держит поток навсегда (UI уже получил «не ответил за 5 с», но поток и процесс остаются); повторные попытки копят бесхозные процессы | settings.rs run_settime (до правки) | `run_cmd_timeout`: spawn + try_wait + deadline + kill; тесты: завершившийся/зависший/несуществующий процесс |
| D2 | P1 | VNC toggle: `.output()` без таймаута, без BUSY — параллельные вызовы, вечный поток при зависании хелпера | settings.rs vnctoggle | `run_cmd_timeout` 5 с + `VNC_BUSY` |
| D3 | P2 | Калибровка без `PRN_BUSY`: параллельно с покупкой этикетка выходит неверной длины; при панике потока BUSY завис (нет catch_unwind) | settings.rs prn-calib | guard `PRN_BUSY`, снятие при любом исходе, тело в `spawn_menu_task` с on_panic |
| D4 | P2 | Тест-печать: `PRN_BUSY` не снимался при панике потока | settings.rs testprint | catch_unwind + снятие BUSY после любого исхода |
| D5 | P3 | diag/export/feed без BUSY — операции самозавершающиеся с таймаутами, двойной запуск безвреден | settings.rs | отложено |

Гонки устаревших результатов: закрыты ранее (REPORT_GEN, op-фильтр печати,
BUSY-guard'ы раскладки/обновления). Завершение приложения: `process::exit(0)`
при перезапуске обрывает потоки, SQLite WAL гарантирует атомарность записи;
незавершённые попытки остаются `ok=NULL` (см. REPRINT_RECOVERY_REPORT).

## 3. Исправления и тесты

- settings.rs: `run_cmd_timeout` (pub(crate)), новый `run_settime`, VNC toggle
  с таймаутом и BUSY, калибровка под `PRN_BUSY` + panic-safe, тест-печать
  panic-safe.
- Новые тесты (187 всего, зелёные):
  - `cmd_timeout_returns_status_of_finished_process`;
  - `cmd_timeout_kills_hanging_process` — зависший процесс убит, вызов не
    держит 30 с;
  - `cmd_timeout_missing_program_is_error`;
  - `repeated_spawn_and_drop_finishes_quickly` — 3 цикла создания/остановки
    весового потока завершаются быстро (join + прерываемый сон).
- Полный набор: 187 passed, 0 failed; release-сборка Linux без предупреждений;
  сервис на стенде активен.

## 4. Оставшиеся ограничения

- Долгоживущие мониторы (net, sync, watchdog, printer) не останавливаются
  никогда — так задумано: они живут вместе с процессом и останавливаются им.
- sync `run_drop` при ошибке спит 60 с между попытками — ограниченный ритм,
  повторов с ростом нагрузки нет.
- diag/export/feed (D5) остаются без BUSY: операции ограничены таймаутами и
  безвредны при двойном запуске.
- Рост логов ограничен logrotate в systemd-юните (внешняя настройка), в
  приложении — нет.

## 5. Отложенные улучшения

- Единый реестр фоновых задач с отменой (ревью ChatGPT P1) — не требуется
  текущими сценариями, локальные guard'ы покрывают всё.
- BUSY для diag/export/feed (D5).
- Проверка под нагрузкой на стенде (многократные циклы меню/отчётов) —
  ручной сценарий.
