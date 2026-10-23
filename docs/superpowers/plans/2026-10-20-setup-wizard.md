# Мастер настройки и чек-лист — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Мастер первичной настройки (8 шагов, без заводского PIN) + динамический чек-лист с кнопкой «Ввести в эксплуатацию».

**Architecture:** Один slint-экран (19) в двух режимах (мастер/чек-лист), состояние в `app_settings`, чистые функции логики в новом `setup.rs`, многопользовательский `auth` (admin id=1, staff id=2), хелпер времени через sudoers.

**Tech Stack:** Rust 1.99.1 (зафиксирован в rust-toolchain.toml), rusqlite/WAL, Slint, systemd.

**Spec:** `docs/superpowers/specs/2026-10-20-setup-wizard-design.md`

## Global Constraints

- PIN: только цифры, длина 4..=8 (формат `auth::verify`, не менять).
- Заводского PIN нет: `seed()` и флаг `pin_changed` удаляются.
- `setup_done`, PIN-ы через `cfgio` не экспортируются.
- UI-поток не блокируется: всё тяжёлое — фоновые потоки с ACK-таймаутами.
- Существующие дедлайны калибровок (150/180 с) не менять.
- Тесты: `cargo test --workspace --target x86_64-pc-windows-msvc`, все зелёные перед коммитом.
- Сборка linux: `cargo zigbuild --release -p openscale --bin kiosk --target x86_64-unknown-linux-gnu.2.41`.
- Комментарии в коде — только для нетривиального (правила AGENTS.md).

## Review Focus

- Пустой `admin_user` при старте → мастер открывается без PIN. Проверка: unit-тест `user_exists` (Task 1) + стенд (Task 6).
- Двойной ввод PIN с несовпадением → ошибка, шаг не двигается. Тест в Task 2 (повтор ввода — логика обработчика, стенд).
- Мусорная длина калибровки <15 мм → не сохраняется (уже реализовано, тест регрессии не нужен — покрыто логикой обработчика, стенд).
- Впрыск аргумента в `openscale-settime` → регэксп+диапазоны в хелпере, ручная проверка на стенде (Task 4).
- Сбой питания между шагами → мастер продолжается с `setup_stage`; запись stage ДО выполнения шага. Тест в Task 2.

---

### Task 1: Многопользовательский auth (admin id=1, staff id=2)

**Files:**
- Modify: `crates/datastore/src/auth.rs` (весь модуль)
- Test: `crates/datastore/src/auth.rs` (модуль tests)

**Interfaces:**
- Consumes: текущая схема `admin_user(id, role, pin_salt, pin_hash)`, `admin_lock(id, failed, locked_until)` — таблицы не меняются (id=2 добавляется строкой).
- Produces (для Task 6):
  - `pub fn user_exists(conn: &Connection) -> Result<bool, rusqlite::Error>` — есть хоть один пользователь.
  - `pub fn create_user(conn: &Connection, id: i64, role: &str, pin: &str) -> Result<Result<(), &'static str>, rusqlite::Error>` — роль `admin`|`staff`, PIN 4..8 цифр, upsert; Err-строка — понятная причина.
  - `pub fn verify(conn: &Connection, pin: &str) -> Result<Auth, rusqlite::Error>` — перебирает ВСЕ строки `admin_user` (упорядоченно по id), возвращает `Granted(Role)` первого совпавшего; формат/лок — как сейчас (глобальный лок `admin_lock` id=1 остаётся общим — осознанное решение: одна клавиатура на двоих).
  - `pub fn pin_is(conn: &Connection, id: i64, pin: &str) -> Result<bool, rusqlite::Error>` — с параметром id.
  - `pub fn change_pin(conn: &Connection, id: i64, old: &str, new: &str) -> ...` — с параметром id.
  - `seed()` удалён. Все вызовы seed/старых сигнатур в `app/kiosk/src/settings.rs` и тестах обновляются.

- [ ] **Step 1:** Переписать тесты: db() без seed; create_user(1,"admin","12345"); verify("12345") == Granted(Admin); create_user(2,"staff","54321") → verify("54321") == Granted(Staff); несовпадение вводов create_user → Err; PIN 3 цифры → Err; change_pin(2, ...) меняет только staff; verify несуществующего PIN → Denied. 
- [ ] **Step 2:** `cargo test -p datastore --target x86_64-pc-windows-msvc` → FAIL (компиляция: нет user_exists/create_user, seed остался).
- [ ] **Step 3:** Реализовать user_exists/create_user; verify — цикл по пользователям (логика лока и штрафа — без изменений, штраф начисляется при полном промахе); pin_is/change_pin — с id; удалить seed; миграция старых БД не нужна (id=1 уже существует, id=2 появится от мастера).
- [ ] **Step 4:** Тесты PASS. Обновить вызовы в `app/kiosk/src/settings.rs` (seed → user_exists/create_user; pin_is → с id=1).
- [ ] **Step 5:** `cargo test --workspace --target x86_64-pc-windows-msvc` → зелёный. Коммит `auth: два пользователя, заводской seed удалён`.

### Task 2: Логика мастера и чек-листа (`app/kiosk/src/setup.rs`)

**Files:**
- Create: `app/kiosk/src/setup.rs`
- Modify: `app/kiosk/src/main.rs` (только `mod setup;`)
- Test: `app/kiosk/src/setup.rs` (модуль tests)

**Interfaces:**
- Consumes: `datastore::get_setting/set_setting` (String), `datastore::get_calibration` (i64).
- Produces (для Task 6/7):
  - `pub const STEP_FINAL: u8 = 8;`
  - `pub enum Action { Next, Back, SkipCashier }`
  - `pub fn advance(stage: u8, action: Action) -> u8` — чистая: Next → min(stage+1, 8); Back → stage.saturating_sub(1), из 1 не выходит; SkipCashier допустим только со stage 2 → 3.
  - `pub fn stage(conn) -> u8` / `pub fn set_stage(conn, u8)` — ключ `setup_stage` (read: parse fail → 1).
  - `pub fn wizard_done(conn) -> bool` / `set_wizard_done(conn, bool)` — ключ `setup_wizard_done`.
  - `pub fn entered(conn) -> bool` / `set_entered(conn, bool)` — ключ `setup_done`.
  - `pub struct Checklist { goods: bool, layout: bool, printer_ok: bool, scales_ok: bool }`
  - `pub fn evaluate(c: Checklist) -> u32` — бит 0=goods,1=layout,2=printer_ok,3=scales_ok.
  - `pub fn all_green(mask: u32) -> bool` — mask == 0b1111.

- [ ] **Step 1:** Тесты: advance последовательность 1→8; Back из 1 → 1; SkipCashier со 2 → 3, с другого — без изменения; evaluate всех комбинаций (5 кейсов: 0, по одному биту, все); all_green только на 0b1111; stage() на битом значении → 1; roundtrip set/ wizard_done/entered.
- [ ] **Step 2:** `cargo test -p openscale --target x86_64-pc-windows-msvc` → FAIL.
- [ ] **Step 3:** Реализовать (тупые обёртки get_setting — parse/запись строкой "0"/"1").
- [ ] **Step 4:** PASS. Коммит `setup: чистая логика мастера и чек-листа`.

### Task 3: cfgio не выносит состояние устройства

**Files:**
- Test: `app/kiosk/src/cfgio.rs` (tests) — если экспорт уже не трогает `app_settings`/`admin_user`, задача сводится к фиксирующему тесту.

**Interfaces:** Consumes: `cfgio::export(cfg) -> Result<PathBuf, String>`. Produces: гарантия для тиража.

- [ ] **Step 1:** Тест: export во временную папку (как в существующих тестах cfgio); прочитать файл; assert: не содержит строк `setup_`, `pin_salt`, `pin_hash`, `label_layout_name`.
- [ ] **Step 2:** Запуск. Если FAIL (экспорт тянет лишнее) — исключить из экспорта; иначе только тест. Коммит `cfgio: состояние устройства (setup/PIN) не экспортируется`.

### Task 4: Дата/время — хелпер, sudoers, конфиг

**Files:**
- Create: `deploy/openscale-settime` (bash, root)
- Create: `deploy/openscale-settime.sudoers`
- Modify: `deploy/README.md` (раздел установки: файлы + строка sudoers)
- Modify: `deploy/openscale.config.toml` (секция `[time]` с комментариями)
- Modify: `app/kiosk/src/config.rs` (struct + default: `time_ntp: bool = true`, `time_server: String = ""`)

**Interfaces:** Produces (для Task 6): киоск вызывает `std::process::Command::new("/usr/local/bin/openscale-settime").arg("<ДД.ММ.ГГГГ ЧЧ:ММ>")` c ожиданием ≤5 с (stdin/stdout — null).

- [ ] **Step 1:** `openscale-settime`: аргумент строго `^[0-9]{2}\.[0-9]{2}\.[0-9]{4} [0-9]{2}:[0-9]{2}$` (регэекс + диапазоны дд 01-31/мм 01-12/ч 00-23/м 00-59), `date -d` валидация реальности даты, затем `timedatectl set-ntp false && timedatectl set-time "YYYY-MM-DD HH:MM:00"`. Иной ввод → exit 1, ничего не менять.
- [ ] **Step 2:** `openscale-settime.sudoers`: `administrator ALL=(root) NOPASSWD: /usr/local/bin/openscale-settime`.
- [ ] **Step 3:** config.rs: поля `time_ntp`/`time_server` + парсинг `[time]`; openscale.config.toml — секция с комментарием «NTP-сервер своей сети; пусто — debian pool».
- [ ] **Step 4:** cargo test (config парсинг) + ручная проверка хелпера на стенде в Task 6. Коммит `deploy: хелпер времени openscale-settime + sudoers + [time] в конфиге`.

### Task 5: Экран 19 — каркас и чек-лист

**Files:**
- Modify: `app/kiosk/ui/main.slint` (экран 19, новые свойства, строка меню)
- Test: нет unit (slint) — сборка + стенд.

**Interfaces:**
- Produces (для Task 6/7): свойства корня: `setup-mode` (0=чек-лист,1=мастер), `setup-stage` (int), `setup-mask` (int), `setup-note` (string), `setup-busy` (bool); колбэк `setup-tap(action: string)`; экран 19 открывается при `root.screen == 19`.

- [ ] **Step 1:** В main.slint: свойства, экран 19: заголовок «НАСТРОЙКА ВЕСОВ», слева список из 8 строк (в чек-листе — 4 пункта) с цветом по состоянию (✓ зелёный / активен / серый), справа контент-заглушка (Text `setup-note`), снизу кнопки: мастер — «Далее/Пропустить» (кассир), чек-лист — «Ввести в эксплуатацию» (активна при `setup-mask == 15`). Строка меню админа «Настройка» (id `setup`).
- [ ] **Step 2:** cargo zigbuild — компиляция slint. Коммит `ui: экран настройки/чек-листа (каркас)`.

### Task 6: Обвязка — старт, шаги мастера, кассир

**Files:**
- Modify: `app/kiosk/src/main.rs` (стартовый выбор экрана)
- Modify: `app/kiosk/src/settings.rs` (handlers `setup-*`)
- Test: ручной стенд (UI); unit — только чистые функции из Task 2.

**Interfaces:**
- Consumes: Task 1 (`user_exists/create_user/verify`), Task 2 (`advance/stage/set_stage/wizard_done/entered/set_entered`), Task 4 (хелпер), Task 5 (`setup-tap`).
- Produces: рабочие шаги мастера.

- [ ] **Step 1:** main.rs: после открытия БД — если `!setup::wizard_done(conn)`: `screen = 19`, `setup-mode` по `user_exists` (0 → сразу в чек-лист-режиме невозможно: если мастера не было, режим мастер). Правка: мастер-режим пока `!wizard_done`.
- [ ] **Step 2:** settings.rs handlers `setup-*` (все фоновые операции — через существующие каналы/потоки):
  - `pin-admin-1/2`, `pin-staff-1/2` — ввод с экрана PIN (повторное использование пин-клавиатуры, фаза в kiosk state); совпало → `create_user(id, role, pin)`, `advance`; не совпало → подсказка, заново.
  - `skip-staff` → `advance(SkipCashier)`.
  - `next` / `back` → advance + set_stage ДО выполнения тела шага (устойчивость к сбою питания).
  - `weights-ok` → оператор подтвердил груз (кнопка активна, когда tick присылает стабильный вес ≥100 г).
  - `print-test` / `calib-feed-ok|tail|far` → печать тестовой этикетки (существующий запрос Test) и правка feed-параметров ±16 (логика `calib::run_cli`, переиспользование `FeedCalibration::save`).
  - `label-calib` → тот же код, что `prn-calib` (вынести в общую fn, не дублировать).
  - `time-apply` (аргумент — строка даты) → `Command` хелпера с таймаутом 5 с, отчёт на экран; `time-ntp-on|off` → `timedatectl set-ntp` через тот же хелпер (добавить в него второй режим: аргумент `ntp-on|ntp-off`).
  - `finish` → `set_wizard_done(true)`, рестарт киоска (существующий `restart_requested`).
  - `enter-service` → `set_entered(true)`, экран в рабочий режим.
- [ ] **Step 3:** Кассир: в обработчике входа по PIN при `role == Staff` вместо админ-меню открывать подменю «протяжка / перезагрузка» (тот же экран меню, пунктам проставить `admin: true`, staff-пунктам `staff: true`; видимость по `ui.get_role()`).
- [ ] **Step 4:** cargo zigbuild + полный тест; деплой на стенд: walkthrough мастера на весах (создание PIN, сеть, груз, печать, калибровки, время, финал) — логи в FIXLOG. Коммит `setup: обвязка шагов мастера и кассирского подменю`.

### Task 7: Чек-лист в тике

**Files:**
- Modify: `app/kiosk/src/tick.rs` (2 поля TickCache + вычисление маски)
- Test: ручной стенд; чистая часть уже покрыта в Task 2.

**Interfaces:**
- Consumes: Task 2 (`evaluate/Checklist/entered/wizard_done`), существующие `snap` (весы), `ph` (принтер).
- Produces: `ui.set_setup_mask(mask)`, `ui.set_setup_mode(mode)` каждый тик.

- [ ] **Step 1:** tick.rs: `goods_count` — не каждый тик: счётчик тиков, пересчёт COUNT(plucodes) раз в 10 с (кэш в TickCache); layout — `datastore::get_setting("label_layout_name")` в том же цикле; printer_ok — `!ph.fault && связь` (существующие поля); scales_ok — `snap.port_up && ноль стабилен` (существующие). Маска только когда `wizard_done && !entered`.
- [ ] **Step 2:** cargo zigbuild + тесты; стенд: галки оживают при синке/импорте. Коммит `tick: маска чек-листа в кэше тика`.

### Task 8: Документация и архив

**Files:**
- Modify: `audit/FIXLOG.md` (стенд-результаты walkthrough)
- Modify: `deploy/README.md` — сверка раздела установки (sudoers/хелпер)
- Modify: `audit/PROTOCOL.md` — пункт «состояния setup».

- [ ] **Step 1:** Зафиксировать результаты стенда, собрать архив `audit/src-bundle/openscale-audit.zip`. Коммит `docs: стенд-результаты мастера настройки`.

## Self-review

- Покрытие спеки: разделы 2 (Task 1,2), 3 (Task 1,6), 4 (Task 2,5,6), 5 (Task 2,5,7), 6 (Task 4,6), 7 (Task 5,6), 8 (Task 6 — дедлайны не тронуты), 9 (тесты в 1,2; стенд в 6,7) — все разделы имеют таски. Гэпов нет.
- Типы: `Auth::Granted(Role)`, сигнатуры Task 1 согласованы с использованием в Task 6; маска u32/`all_green` едины в Task 2/5/7.
- Пропорция: план ~соизмерим со спекой, тела не прописаны кроме алгоритма хелпера.
