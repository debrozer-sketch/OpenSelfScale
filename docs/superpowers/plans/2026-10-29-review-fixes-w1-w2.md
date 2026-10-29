# План исправлений по ревью openscale-review.md — Волны 1–2

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** устранить дефекты P0/P1 и быстрые дыры безопасности из `docs/openscale-review.md`, не ломая инварианты «программа не зависает».

**Architecture:** точечные исправления в существующих модулях (deploy-скрипты, crates/shtrih-m, crates/datastore, app/openscale: sync, flow, tick, escpos) без рефакторинга. UI-перестройка (разделы 6–7 ревью) — отдельные планы после Волн 1–2.

**Tech Stack:** Rust (workspace), Slint, bash/systemd-скрипты, SQLite (rusqlite).

**Spec:** `docs/openscale-review.md` — номера B*/S* ниже ссылаются на него.

## Global Constraints

- Инварианты «программа не зависает» из AGENTS.md: UI-поток не блокируется; у каждого блокирующего вызова таймаут; у каждого ожидания предельный срок; паника фонового потока не убивает функцию; внешние ресурсы не держатся надолго; экран ожидания разрешается.
- В коде нет комментариев (кроме объяснения нетривиального), нет TODO/FIXME.
- Секреты только в `.env` (не в git); хардкоды IP/путей запрещены.
- Сборка/деплой: `cargo test --workspace --target x86_64-pc-windows-msvc`; релиз `cargo zigbuild --release -p openscale --bin openscale --target x86_64-unknown-linux-gnu.2.41` — деплой только после `Finished`; деплой `python deploy\push.py …` + install/systemctl через `python scale_ssh.py --sudo`.
- Каждый task: тесты зелёные → коммит → (для задач с эффектом на стенде) деплой и проверка на устройстве.
- Файлы для Linux (`deploy/**`) — только LF, фиксируется в `.gitattributes`.

## Review Focus

- Разрыв связи с весами на полуслове: линия шлёт мусорные байты → драйвер завершается по общему дедлайну, а не по таймауту каждого read (тест в T8).
- Синхронизация во время приёма выгрузки: источник перезаписал plu.xml во время обработки → удалить можно только ровно то, что прочитано (тест в T10).
- Часы ушли в 1970 после отключения питания → блокировка PIN не длится «десятилетия» (тест в T5).
- Смена цены между подтверждением покупателя и печатью → печатается цена из снимка карточки (тест в T9).
- Сетевой сбой между транзакциями приёма → каталог не остаётся наполовину обновлённым (тест в T10).

---

# Дорожная карта (все волны)

| Волна | Содержание | Статус |
|---|---|---|
| **1. Сразу** | B1 settime, B2 LF, B8 цены, S4 управляющие байты, S5 ntp-server, B11 lockout, S2 VNC, S1 root-доступ | **этот план** |
| **2. Неделя** | B3 дедлайн драйвера + возраст снимка, B4 снимок цены, B6/B7 приём выгрузки, B9/B13 автовыход админа и go_root, B10 скрытый вход, B14 дубли страниц, B15 отмена печати, B17 UTC-время отчёта | **этот план** |
| 3. Данные и группы | B18/B19 единый источник групп/отделов, B20 тихие потери строк, B21 коллизии EAN-13, B24 границы настроек | отдельный план |
| 4. UI-основа | Раздел 6.1: Theme, AppButton/MenuRow/ScreenHeader, таблица экранов, ScrollView | отдельный план |
| 5. UI-покупатель и отзывчивость | Разделы 6.2–6.4 и 6.3 (вынос блокировок из UI-потока), B16 лоток, B22 дебаунс, B23 обнуление, B25 пул потоков | отдельный план |
| 6. Меню и сервис | Раздел 7: новая структура меню, журнал действий, состояние | отдельный план |
| 7. Безопасность глубже | S3 TLS/подпись выгрузки, S6 KDF, S7/S8/S9/S10/S11 | отдельный план |

---

# Волна 1

### Task 1: `.gitattributes` — LF для deploy и скриптов (B2)

**Files:**
- Create: `.gitattributes`

**Interfaces:**
- Produces: гарантия, что `git archive`/checkout отдают `deploy/**` и скрипты в LF независимо от платформы.

- [ ] **Step 1: создать `.gitattributes`**

```
* text=auto eol=lf
*.png binary
*.jpg binary
*.zip binary
*.pdf binary
*.xz binary
```

- [ ] **Step 2: нормализовать дерево**

`git add --renormalize .` — диф по содержимому должен остаться пустым (блобы уже LF).

- [ ] **Step 3: проверить, что archive отдаёт LF**

Run: `git archive --format=zip -o $env:TEMP\opencode\lftest.zip HEAD; python -c "import zipfile; print(zipfile.ZipFile(r'$env:TEMP\opencode\lftest.zip').read('deploy/openscale-settime').count(b'\r\n'))"`
Expected: `0`

- [ ] **Step 4: Commit**

```bash
git add .gitattributes
git commit -m "repo: eol=lf в .gitattributes — archive/checkout не портит deploy-скрипты"
```

### Task 2: восстановить `openscale-settime` + валидация ntp-server (B1, S5)

**Files:**
- Modify: `deploy/openscale-settime` (переписать целиком)
- Modify: `deploy/openscale-settime.sudoers` — ограничить аргументы шаблоном

**Interfaces:**
- Consumes: `settime.rs` (app/openscale) вызывает `openscale-settime "<ДД.ММ.ГГГГ ЧЧ:ММ>"`, `ntp-on`, `ntp-off`, `ntp-server <host>`.
- Produces: рабочий скрипт; все ветки проходят `bash -n`; host проверяется маской.

- [ ] **Step 1: написать скрипт заново**

1. Аргумент-дата `ДД.ММ.ГГГГ ЧЧ:ММ` → `date -s` (конвертация формата внутри), `hwclock -w` если доступен.
2. `ntp-on` → `timedatectl set-ntp true` + `systemctl restart systemd-timesyncd`.
3. `ntp-off` → `timedatectl set-ntp false`.
4. `ntp-server <host>` → валидация `^[A-Za-z0-9._-]{1,253}$` (grep -E), иначе отказ; `printf '[Time]\nNTP=%s\n'` в `/etc/systemd/timesyncd.conf.d/openscale.conf` + restart timesyncd.
5. Неизвестный аргумент → usage в stderr, exit 1.

- [ ] **Step 2: проверить синтаксис локально**

Run: `& "C:\Program Files\Git\bin\bash.exe" -n deploy/openscale-settime`
Expected: без вывода (exit 0)

- [ ] **Step 3: sudoers по шаблону**

Ограничить аргументы: дата-строка в кавычках, `ntp-on`, `ntp-off`, `ntp-server` + аргумент. Сверить с реальными вызовами из `settime.rs`.

- [ ] **Step 4: деплой на стенд и живая проверка**

```bash
python deploy\push.py deploy\openscale-settime deploy\openscale-settime.sudoers
python scale_ssh.py --sudo "install -m 755 /home/administrator/install/openscale-settime /usr/local/bin/ && install -m 440 /home/administrator/install/openscale-settime.sudoers /etc/sudoers.d/ && visudo -c && bash -n /usr/local/bin/openscale-settime && echo OK"
```
Expected: `visudo -c` без ошибок, `OK`.

- [ ] **Step 5: Commit**

```bash
git add deploy/openscale-settime deploy/openscale-settime.sudoers
git commit -m "deploy: восстановлен openscale-settime, валидация ntp-server, sudoers по шаблону"
```

### Task 3: валидация цен на импорте и насыщающая сумма (B8)

**Files:**
- Modify: `crates/datastore/src/lib.rs` (импорт XML/JSON: отказ `price <= 0` и `price > 99_999_999`)
- Modify: `app/openscale/src/session.rs` (`sum_kopecks` → saturating)
- Test: `crates/datastore/src/lib.rs`

**Interfaces:**
- Consumes: `datastore::import_plu` (XML-путь), JSON-путь импорта, `session::sum_kopecks(price, weight_g) -> i64`.
- Produces: импорт отклоняет некорректные цены с текстом ошибки; `sum_kopecks` не переполняется.

- [ ] **Step 1: failing-тесты в datastore**

```rust
#[test]
fn import_rejects_nonpositive_and_huge_prices() { /* XML price="0"/"-1"/"100000000" → Err; "47999" → Ok */ }
#[test]
fn json_import_rejects_nonpositive_and_huge_prices() { /* то же для JSON-пути */ }
```

- [ ] **Step 2: убедиться, что падают**

Run: `cargo test -p datastore --target x86_64-pc-windows-msvc`
Expected: FAIL

- [ ] **Step 3: реализовать**

В обоих путях импорта перед вставкой: `if price <= 0 || price > 99_999_999 { return Err(...) }` («цена позиции X вне диапазона»).

- [ ] **Step 4: saturating сумма в session**

`sum_kopecks`: `saturating_mul` вместо `*` (тип по факту). Тест: `sum_kopecks(99_999_999, 99_999)` не паникует.

- [ ] **Step 5: тесты зелёные + Commit**

Run: `cargo test -p datastore -p openscale --target x86_64-pc-windows-msvc`
Expected: PASS

```bash
git add crates/datastore/src/lib.rs app/openscale/src/session.rs
git commit -m "datastore: отсечение цен <=0 и >99 999 999 на импорте; saturating-сумма"
```

### Task 4: фильтр управляющих байтов в печати (S4)

**Files:**
- Modify: `crates/escpos/src/lib.rs` (`to_cp866`)
- Modify: `crates/datastore/src/lib.rs` или `app/openscale/src/sync.rs` (зачистка названий на входе)
- Test: `crates/escpos/src/lib.rs`

**Interfaces:**
- Consumes: `to_cp866` (сигнатура по коду).
- Produces: `to_cp866` заменяет все символы `< 0x20` на пробел; названия из выгрузки хранятся уже чистыми.

- [ ] **Step 1: failing-тест**

```rust
#[test]
fn to_cp866_strips_control_bytes() {
    let out = to_cp866("A\u{1b}B\u{3}C").unwrap();
    assert_eq!(out, b"A B C");
}
```

- [ ] **Step 2: убедиться, что падает**

Run: `cargo test -p escpos --target x86_64-pc-windows-msvc to_cp866`
Expected: FAIL

- [ ] **Step 3: реализовать**

В `to_cp866` перед кодированием: символы `< 0x20` → пробел. В импорте выгрузки — тот же фильтр для названий (тест: название с `\u{1b}` сохраняется как `A B`).

- [ ] **Step 4: тесты зелёные + Commit**

Run: `cargo test --workspace --target x86_64-pc-windows-msvc`
Expected: PASS

```bash
git add crates/escpos/src/lib.rs crates/datastore/src/lib.rs app/openscale/src/sync.rs
git commit -m "escpos/datastore: фильтр управляющих байтов — ESC-инъекция через названия исключена"
```

### Task 5: блокировка PIN — потолок 600 с и сбитые часы (B11)

**Files:**
- Modify: `crates/datastore/src/auth.rs` (`admin_lock`: вычисление wait, запись locked_until)
- Test: `crates/datastore/src/auth.rs`

**Interfaces:**
- Consumes: схема `admin_lock id=1`, поле `locked_until` (unix-секунды).
- Produces: wait не более 600 с; при системном времени раньше 2024-01-01 блокировка считается истёкшей.

- [ ] **Step 1: failing-тесты**

```rust
#[test]
fn lock_wait_never_exceeds_600s() { /* locked_until = now + 10_000 → wait == 600 */ }
#[test]
fn lock_expired_when_clock_before_2024() { /* locked_until = 60_000 → wait == 0 */ }
```

- [ ] **Step 2: убедиться, что падают**

Run: `cargo test -p datastore --target x86_64-pc-windows-msvc lock_`
Expected: FAIL

- [ ] **Step 3: реализовать**

`wait = remaining.clamp(0, 600)`; перед этим `if now_epoch() < 1_700_406_400 { return 0 }`. При записи: `locked_until = now + min(next_wait, 600)`.

- [ ] **Step 4: тесты зелёные + Commit**

Run: `cargo test -p datastore --target x86_64-pc-windows-msvc`
Expected: PASS

```bash
git add crates/datastore/src/auth.rs
git commit -m "auth: lockout не длиннее 600 с, сбитые часы не продлевают блокировку"
```

### Task 6: VNC только на localhost (S2)

**Files:**
- Modify: `deploy/openscale-vnc.service`

**Interfaces:**
- Produces: x11vnc слушает 127.0.0.1; доступ снаружи — только SSH-туннель.

- [ ] **Step 1: правка unit-файла**

В `ExecStart` добавить `-localhost`, остальное без изменений.

- [ ] **Step 2: деплой и проверка**

```bash
python deploy\push.py deploy\openscale-vnc.service
python scale_ssh.py --sudo "install -m 644 /home/administrator/install/openscale-vnc.service /etc/systemd/system/ && systemctl daemon-reload && systemctl restart openscale-vnc && ss -tlnp | grep 5900"
```
Expected: `127.0.0.1:5900`. Работоспособность туннеля проверить визуально с рабочей машины.

- [ ] **Step 3: Commit**

```bash
git add deploy/openscale-vnc.service
git commit -m "deploy: x11vnc только на localhost"
```

### Task 7: root-доступ по ключу, парольный вход выключен (S1)

**Files:**
- Modify: `deploy/preseed.cfg` (late_command: authorized_keys, sshd_config)
- Modify: `deploy/make_initrd.py` (подстановка pubkey из `.env`)
- Modify: `deploy/README.md` (инструкция `SCALE_SSH_PUBKEY`)
- Modify: `.env` локально (не коммитится) — `SCALE_SSH_PUBKEY=…`

**Interfaces:**
- Consumes: текущий механизм подстановки root-пароля в preseed через make_initrd.
- Produces: новые установки принимают SSH root только по ключу; `scale_ssh.py` уже ключевой — доступ сохраняется.

- [ ] **Step 1: механизм подстановки**

Прочитать `deploy/make_initrd.py`, в месте подстановки пароля добавить pubkey: `echo '<PUBKEY>' > /root/.ssh/authorized_keys && chmod 600 ... && chown -R root:root /root/.ssh` в late_command. Если `SCALE_SSH_PUBKEY` пуст — сборка падает с понятной ошибкой (не молча оставлять парольный вход).

- [ ] **Step 2: sshd конфиг**

В late_command: `printf 'PermitRootLogin prohibit-password\nPasswordAuthentication no\n' > /etc/ssh/sshd_config.d/99-openscale.conf`.

- [ ] **Step 3: README**

Инструкция: ключ задаётся до make_initrd; следующая установка — только по ключу. Проверка live отложена до переустановки стенда.

- [ ] **Step 4: Commit**

```bash
git add deploy/preseed.cfg deploy/make_initrd.py deploy/README.md
git commit -m "deploy: ssh по ключу, парольный вход root выключен (S1)"
```

---

# Волна 2

### Task 8: дедлайн драйвера весов и возраст снимка (B3)

**Files:**
- Modify: `crates/shtrih-m/src/lib.rs` (`poll_once`: общий дедлайн, ACK после LRC, `TIOCEXCL`, `O_NONBLOCK`)
- Modify: `app/openscale/src/weight.rs` (`updated_at` в состоянии потока)
- Modify: `app/openscale/src/tick.rs` (снимок старше 1500 мс → `port_up = false`)
- Test: `crates/shtrih-m/src/lib.rs`

**Interfaces:**
- Produces: `poll_once` завершается не позднее `DEADLINE_MS = 1000` от начала вызова при любом потоке байтов; ACK пишется только после проверки LRC; снимок несёт `updated_at_ms` (мс от эпохи `Instant` процесса: `INSTANT_EPOCH: OnceLock<Instant>`); `tick` считает снимок протухшим при `age > 1500`.

- [ ] **Step 1: failing-тесты**

Выделить цикл чтения в функцию над трейтом `PortRead { fn read_some(&mut self, buf: &mut [u8]) -> io::Result<usize> }` (обёртка SerialPort реализует трейт).

```rust
#[test]
fn poll_deadline_on_garbage_stream() { /* mock вечно отдаёт по 1 байту мусора → Err за <= DEADLINE_MS */ }
#[test]
fn ack_sent_only_after_lrc_ok() { /* битый LRC → ACK не записан; корректный кадр → ACK записан */ }
```

- [ ] **Step 2: убедиться, что падают**

Run: `cargo test -p shtrih-m --target x86_64-pc-windows-msvc`
Expected: FAIL

- [ ] **Step 3: реализовать драйвер**

- `poll_once`: `deadline = Instant::now() + DEADLINE_MS`; выход из цикла чтения по превышению с ошибкой «нет кадра за дедлайн».
- ACK: буферизовать кадр → LRC → записать ACK.
- Открытие порта: `O_NONBLOCK | O_NOCTTY`, `ioctl(TIOCEXCL)`; убедиться, что libc уже в зависимостях крейта (добавить при необходимости — правило adding-dependencies: сначала проверить существующее).

- [ ] **Step 4: возраст снимка**

`weight.rs`: `updated_at_ms` обновляется при каждом успешном `poll_once`. `tick.rs`: `age > 1500` → `port_up = false`, вес «—» до следующего успешного опроса.

- [ ] **Step 5: тесты зелёные**

Run: `cargo test -p shtrih-m -p openscale --target x86_64-pc-windows-msvc`
Expected: PASS

- [ ] **Step 6: деплой и проверка на стенде**

Подать мусор в линию (CLI на другом процессе должен получить отказ порта после TIOCEXCL), выдернуть/вернуть линию весов: статус связи восстанавливается, этикетка со старым весом не выходит.

- [ ] **Step 7: Commit**

```bash
git add crates/shtrih-m/src/lib.rs app/openscale/src/weight.rs app/openscale/src/tick.rs
git commit -m "shtrih-m/weight: общий дедлайн poll_once, ACK после LRC, TIOCEXCL, возраст снимка"
```

### Task 9: печать по снимку карточки (B4)

**Files:**
- Modify: `app/openscale/src/context.rs` (KioskState: `selected_item: RefCell<Option<PluItem>>`)
- Modify: `app/openscale/src/flow.rs` (при `plu-tapped` сохранить `PluItem` в kiosk)
- Modify: `app/openscale/src/print.rs` (`print_for_item(cfg, item, …)`)
- Modify: `app/openscale/src/tick.rs` (подтверждение печатает из снимка)
- Test: `app/openscale/src/print.rs`

**Interfaces:**
- Produces: `print_for_item(cfg, item: &PluItem, weight_g, date, time, rounding) -> Result<(), PrintError>` — этикетка из снимка, БД для позиции не читается; `print_for_plu` остаётся для CLI.

- [ ] **Step 1: failing-тест**

```rust
#[test]
fn print_for_item_matches_build_label() {
    // build_label(item, …) == байты, собираемые print_for_item при printer.enabled=false? — проверить по контракту PrintError::PrinterDisabled
}
```

- [ ] **Step 2: реализовать**

- `plu-tapped`: `PluItem` карточки клонируется в `kiosk.selected_item`.
- Подтверждение: печать из снимка; `None` → отказ (нормально не случается).
- Печать не открывает БД для позиции.

- [ ] **Step 3: тесты зелёные + Commit**

Run: `cargo test -p openscale --target x86_64-pc-windows-msvc`
Expected: PASS

```bash
git add app/openscale/src/context.rs app/openscale/src/flow.rs app/openscale/src/print.rs app/openscale/src/tick.rs
git commit -m "print: печать из снимка карточки — цена подтверждена покупателем"
```

### Task 10: атомарный приём выгрузки без гонок (B6, B7)

**Files:**
- Modify: `app/openscale/src/sync.rs` (`run_drop`, `apply_upload`)
- Test: `app/openscale/src/sync.rs`

**Interfaces:**
- Produces: `apply_upload` — одна транзакция (import_plu + remove_absent + apply_layout); `run_drop` удаляет только файлы, которые прочитал, при неизменившихся размере/mtime; межпроцессный lock-файл на каталог обмена (create_new), CLI `--sync` берёт тот же lock.

- [ ] **Step 1: failing-тесты**

```rust
#[test]
fn drop_keeps_rewritten_files() { /* обработали старый plu.xml, на диске новый (другой размер) → файлы НЕ удалены, приём отложен */ }
#[test]
fn drop_removes_only_read_set() { /* посторонний файл в каталоге переживает очистку */ }
#[test]
fn sync_lock_is_exclusive() { /* второй run_drop при удержанном lock → Err(занято) */ }
```

- [ ] **Step 2: убедиться, что падают**

Run: `cargo test -p openscale --target x86_64-pc-windows-msvc drop_`
Expected: FAIL

- [ ] **Step 3: реализовать**

- До обработки: зафиксировать `(path, size, mtime)` каждого файла выгрузки.
- После применения: перепроверить size+mtime; совпало → удалить; нет → не удалять, пометить «отложено, повторный приём».
- Удаление только из зафиксированного списка.
- Lock: файл `sync.lock` в каталоге обмена, `OpenOptions::new().create_new(true)`; удерживается на весь run_drop; занят → выход с журналом.
- `apply_upload`: одна транзакция SQLite на import_plu + remove_absent + apply_layout.

- [ ] **Step 4: тесты зелёные + деплой + проверка**

На стенде: повторный приём той же выгрузки, приём при «недописанном» файле (положить половину XML), CLI `--sync` параллельно с киоском — ни одного частичного каталога, ни одной потери новой выгрузки.

- [ ] **Step 5: Commit**

```bash
git add app/openscale/src/sync.rs
git commit -m "sync: атомарный приём, flock каталога, удаление только прочитанного"
```

### Task 11: автовыход администратора и честный go_root (B9, B13)

**Files:**
- Modify: `app/openscale/src/context.rs` (KioskState: `admin_since_ms: Option<u64>` или переиспользование idle)
- Modify: `app/openscale/src/flow.rs` (`go_root` → функция `go_root_state(kiosk: &RefCell<…>, ui: &MainWindow)`, сбрасывающая всё в одном borrow)
- Modify: `app/openscale/src/tick.rs` (админ-сессия > 120 с без касаний → go_root_state; сброс `header_dept_page`)

**Interfaces:**
- Produces: `go_root_state` сбрасывает phase/selected/print_op/header_dept_page/popup/search в одном borrow_mut; `tick` вызывает её без `try_borrow_mut`; админ-сессия закрывается по таймеру 120 с (таймер тикает только когда `admin_open`).

- [ ] **Step 1: тест на сброс состояния**

Логика сброса — чистая функция `reset_state(state: &mut KioskState)` → вызывается из go_root_state. Тест: заполненное состояние после reset — дефолтное (header_dept_page == 0).

- [ ] **Step 2: реализовать**

- `reset_state` в tick.rs или context.rs.
- `go_root_state`: один `borrow_mut`, внутри reset + экран 0; `tick` вызывает её, `flow::go_root` — обёртка для UI-колбэков.
- Админ-таймер: при `admin_open` и отсутствии касаний 120 с → `go_root_state` + запись в лог.

- [ ] **Step 3: тесты зелёные + деплой + проверка**

На стенде: открыть админку, не трогать 2 минуты → главный экран; «ДАЛЕЕ» в каталоге больше не прыгает после простоя.

- [ ] **Step 4: Commit**

```bash
git add app/openscale/src/context.rs app/openscale/src/flow.rs app/openscale/src/tick.rs
git commit -m "tick/flow: автовыход админа 120 с, go_root сбрасывает всё состояние в одном borrow"
```

### Task 12: скрытый вход — долгое нажатие на логотип (B10)

**Files:**
- Modify: `app/openscale/ui/main.slint` (TouchArea логотипа: `pointer-event` зажатия; убрать tap-зоны admin-pop-zone)
- Modify: `app/openscale/src/tick.rs` (кнопка удерживается ≥ 3000 мс подряд → экран PIN)
- Modify: `app/openscale/src/settings.rs` (удалить tap-зону в карточке товара)

**Interfaces:**
- Produces: PIN-экран открывается только удержанием логотипа 3 с; серия касаний по весу PIN не открывает.

- [ ] **Step 1: механика удержания**

Slint: у `logo-ta` свойство `pressed` уже существует — в tick считать «зажато и непрерывно с момента X»; `pointer-event` (Pressed/Released) фиксирует границы. Окно серии исключено: требуется непрерывное удержание.

- [ ] **Step 2: удалить старые зоны**

`admin-pop-zone` (карточка товара) и зона 5 нажатий на области веса — убрать; обработчик скрытого входа остаётся один.

- [ ] **Step 3: деплой + проверка**

На стенде: пять случайных тапов по весу — ничего; удержание логотипа 3 с — PIN-экран.

- [ ] **Step 4: Commit**

```bash
git add app/openscale/ui/main.slint app/openscale/src/tick.rs app/openscale/src/settings.rs
git commit -m "ui: скрытый вход — удержание логотипа 3 с, случайные касания не открывают PIN"
```

### Task 13: отмена печати доходит до демона (B15)

**Files:**
- Modify: `app/openscale/src/print.rs` (`PrinterRequest::Print` получает `cancel: Arc<AtomicBool>`)
- Modify: `app/openscale/src/tick.rs` (таймаут 30 с выставляет cancel, а не только статус)

**Interfaces:**
- Produces: `PrinterRequest::Print { bytes, ack, queued_at, meta, cancel }`; поток печати проверяет `cancel.load()` перед началом отправки и между чанками; отменённое задание не печатается и не инкрементирует журнал.

- [ ] **Step 1: реализовать**

- В `request()` для Print создавать `Arc<AtomicBool>`, возвращать наружу (или хранить в очереди демона по op-номеру).
- tick при таймауте: `cancel` по номеру операции; демон перед отправкой и между чанками проверяет флаг; отменённое задание → статус «отменено» в журнал.

- [ ] **Step 2: деплой + проверка**

На стенде: заблокировать подачу бумаги (имитация зависания печати), дождаться ошибки → убедиться, что этикетка не выходит после сообщения об ошибке; нормальная печать не страдает.

- [ ] **Step 3: Commit**

```bash
git add app/openscale/src/print.rs app/openscale/src/tick.rs
git commit -m "print: отмена задания доходит до демона — этикетка не выходит после ошибки"
```

### Task 14: дубли страниц каталога при выключенной кнопке «Назад» (B14)

**Files:**
- Modify: `app/openscale/src/tiles.rs` (`items_pages(total, cap, back)`)
- Modify: `app/openscale/src/flow.rs` (`show_items_page`, обработчик `more`)
- Test: `app/openscale/src/tiles.rs`

**Interfaces:**
- Produces: при `back=false`: страница 0 = `cap-1` товаров, далее шаг и размер = `cap-1`; при `back=true`: как сейчас (`cap-1`, затем `cap-2` с ячейкой «← НАЗАД»). `items_pages` согласована с вёрсткой в обоих режимах.

- [ ] **Step 1: failing-тесты**

```rust
#[test]
fn pages_no_back_step_matches_take() {
    // cap=27, back=false: items_pages(52, 27, false) = 2 (26 + 26);
    // show-логика: page1 = items[26..52], без дублей
}
#[test]
fn pages_with_back_unchanged() { /* cap=27, back=true: прежние значения 1/2/3 */ }
```

- [ ] **Step 2: убедиться, что падают**

Run: `cargo test -p openscale --target x86_64-pc-windows-msvc items_pages`
Expected: FAIL

- [ ] **Step 3: реализовать**

- `show_items_page`: `let step = if back { cap - 2 } else { cap - 1 }`; `skip = (cap - 1) + (page - 1) * step`; `take = step`.
- `items_pages(total, cap, back)` с соответствующим шагом.
- Обновить `on_more` (тот же расчёт).

- [ ] **Step 4: тесты зелёные + Commit**

Run: `cargo test -p openscale --target x86_64-pc-windows-msvc`
Expected: PASS

```bash
git add app/openscale/src/tiles.rs app/openscale/src/flow.rs
git commit -m "flow: без кнопки «Назад» страницы каталога без дублей, пагинация согласована"
```

### Task 15: локальное время в отчёте печати (B17)

**Files:**
- Modify: `app/openscale/src/print.rs` (`local_hms` → `localtime_r`, как в этикетке)

**Interfaces:**
- Produces: отчёт по печати показывает локальное время (совпадает с этикеткой и границами периодов).

- [ ] **Step 1: реализовать**

`local_hms` использует `libc::localtime_r` (паттерн уже есть в `utc_stamp`/этикетке — переиспользовать).

- [ ] **Step 2: проверка**

На стенде распечатать отчёт: время совпадает с часами весов.

- [ ] **Step 3: Commit**

```bash
git add app/openscale/src/print.rs
git commit -m "print: отчёт печати по локальному времени"
```

---

# Верификация волны (после всех задач)

- [ ] `cargo test --workspace --target x86_64-pc-windows-msvc` — все зелёные.
- [ ] `cargo zigbuild --release …` — `Finished`, деплой, `systemctl is-active openscale-kiosk` = active.
- [ ] На стенде: взвешивание → печать → сверка цены/веса; выдёргивание линии весов; удержание логотипа; админ-таймаут 2 мин; повторный приём выгрузки.
- [ ] Обновить леджер `.superpowers/sdd/…/progress.md` и этот файл (чекбоксы).



---

# Итог исполнения (перепроверка 2026-10-29, вторым проходом)

Волны 1–2 выполнены: T1–T15. При перепроверке найдены и исправлены два дефекта исполнения:
1. T12-регрессия: зона удержания логотипа требовала logo-has — без logo.png админка была недоступна. Исправлено: зона живёт на screen 0 всегда (невидима без логотипа, но работает).
2. Мёртвый код зоны 5 тапов (on_admin_tap, Kiosk.admin_taps/admin_tap, callback admin-tap) — снесён; заодно исчез один из вызовов net::read_live в UI-потоке (инвариант 1).

Корректность T9 подтверждена: refresh_data (flow.rs, блок «карточка PLU закрыта») закрывает popup при изменении товара синхронизацией — рассинхрон «видел в карточке / напечатано» невозможен.

Проверки: cargo test --workspace (windows target) — 125+ passed, 0 failed; zigbuild release — Finished; стенд: settime bash -n + инъекция отвергнута, VNC localhost, SSH key/password оба пути.

## Остатки волн 1–2 (перенос в новые планы)

- **R1 (B1, остаток, высокий)**: мастер всё ещё продвигается при ошибке хелпера settime (spawn_settime пишет только заметку). Сделать: шаг времени мастера применяет хелпер до advance; при ошибке — остаёмся на шаге с заметкой.
- **R2 (B8, гигиена)**: XML cardprice=0 даёт Some(0), JSON даёт None — унифицировать (0 → None), проверить отображение «цены по карте».
- **R3 (B11, остаток)**: блокировка на SystemTime (не монотонные часы). Компенсировано потолком 600 с и sanity-эпохой; добить при переработке auth (S6).
- **R4 (S1, остаток)**: сервисный пользователь вместо root-по-ключу — волна 7.
- **R5 (S2, остаток)**: выключение VNC из меню админки (сейчас только -localhost в unit) — волна 6/7.

## Дорожная карта дальше (по ревью: раздел 8 + разделы 2–7)

- **Волна 3. Данные**: B18 (единый источник групп: groupcode из layout.xml не используется UI; UPPER без ICU), B19 (SEED_GROUP_DEPT оживает после удаления), B20 (тихие потери строк filter_map(r.ok()) — логировать), B21 (коллизии EAN-13), B24 (границы настроек 1..10 vs 1..5; экспорт без привязок/раскладок), B5 (округление: предупреждение при включении + запись в журнал).
- **Волна 4. UI-основа (раздел 6.1)**: Theme (43→12 цветов, шкала шрифтов), AppButton/MenuRow/ScreenHeader/Pager/ToggleRow/StepperRow, лэйауты вместо координат (убрать литералы 992/1008/840 и дубли вёрстки Rust/Slint), ScrollView/ListView для меню, единая таблица Screen{id,title,parent} вместо admin_close/depts_close/tick-таблиц, иконки Path, @tr().
- **Волна 5. Отзывчивость (инварианты раздела 2)**: I1 — refresh_data/relayout/verify/read_live/ensure_tables из UI-потока; tick: DB+lock_left каждые 100 мс. I2 — лимит размера FTP retr_as_buffer и read_to_string(plu.xml). I4 — паники потоков меню (NET_APPLYING, калибровка, диагностика). I5 — PRAGMA synchronous/wal, busy_timeout, кэш соединений. I6 — таймаут экранов «Собираю…». Плюс: B16 (блокировка печати до очистки лотка), B22 (дебаунс мастера: тест-печать, +/- подачи, калибровка), B23 (zero: проверка пустой платформы + журнал), B25 (пул потоков вместо spawn на действие), миниатюры картинок и кэш по байтам, debounce поиска.
- **Волна 6. Меню (раздел 7)**: структура 9 разделов админа, плоские плитки кассира (состояние, обнулить с проверкой, протяжка, тест, журнал печати, мой PIN), журнал действий, B12 (восстановление доступа) — сюда же.
- **Волна 7. Безопасность**: S3 (TLS/подпись выгрузки), S6 (argon2id/PBKDF2, БД 0600 + R3), S7 (cfg-каталог вне FTP-зоны, полная сводка импорта), S8 (imgsync: пароль не в argv, проверка размера/содержимого картинок), S9 (IP на экране покупателя), S10 (sudoers-шаблоны аргументов), S11 (run_check.py: /tmp-скрипт под sudo, host key), R4, R5.
- **Гигиена (P2/P3, между волнами)**: to_cp866 не знает №/°/«» (молча «?»), EscPosError::Encoding недостижим; raster() высота >255 усекается; три копии календарной математики; сортировка по байтам (Ё, строчные); net.rs prefix_to_mask>32, IFACE зашит в 4 местах; fsync в openscale-net-apply; scale_ssh.py: ключ scalefw_bastion vs openscale_bastion, только Ed25519; run_check.py: неиспользуемая переменная, пароль через split, файл не закрывается; tools/label-designer не собирается (нет package.json), дубль EAN-13; README/архив: лишние каталоги, __pycache__; мёртвый код раздела 4 (setup::evaluate, dept_of, print_log_count_*, print_log_recent, parse_status/PrinterStatus, ui.max_cols, imp-popup-ok/labels-popup-ok, ticks/goods, Mutex в однопоточном коде, дубли меню-страниц); config.rs: KNOWN_SECTIONS без present_wait_secs, Config::load падает без config.toml; свайп 60–80 px мёртвая зона; контрасты 3:1/2:1 и технические тексты покупателю (разделы 6.2/6.4).


## Исполнение остатков (2026-10-29, сессия 2)

- **R1** (cfbf112): run_settime/spawn_settime/spawn_settime_then_advance — мастер времени применяет хелпер до advance; при ошибке остаётся на шаге с заметкой. thread_local KIOSK_RC для мутации kiosk из invoke.
- **R2**: закрыт без правок — SQL import_plu уже нормализует cardprice=0 → NULL.
- **B5** (de0224f): включение округления показывает глобальную модалку imp-popup с предупреждением «касса считает сама».
- **B20** (df3e3b3): rows_collect_warn — потерянные строки запросов логируются (5 функций datastore).
- **B19** (0408fbd): сидинг под маркером app_settings.seed_done — удалённое не возрождается при перезапуске.
- **B18** (09cba7c): группа товара вычисляется в Rust (полный Unicode-uppercase) при импорте + разовая нормализация БД (group_norm_v2). groupcode layout.xml — ограничение: перевод требует UI-привязок (волна 6).
- **B21** (ba5a04a): ean_code5_collisions — коллизии штрихкодов обнаруживаются при импорте, warn в журнал.
- **B24** (c6dabf0): границы колонок 1..10 едины; экспорт/импорт привязок групп и раскладок этикеток; найден и исправлен предсуществующий баг dept_reorder (вложенная транзакция ломала импорт с отделами).

Проверки: cargo test --workspace windows — все suites ok (43+2+16+83+12…), релиз zigbuild x86_64-linux.2.41 Finished. Стендовая проверка мастера settime — остаётся для волны верификации.

## Волна 6 (меню) — транш 1: выполнен

- Журнал действий: `action_log` (ротация 1000), запись в save_setting/zero/сеть/импорт/PIN; экран slint 23/stage 22, пункт в Обслуживании (коммиты 7d11e41, 8033827).
- Права меню в Rust: Kiosk.role, гейт ADMIN_TAPS в on_menu_tap (114e199).
- B12 частично: смена PIN по роли, «Мой PIN» кассира (65dabf0).
- Кассир 6 пунктов + экран «Состояние» slint 24/stage 23 (a3867fc).
- Ruling: сервисный код B12 (HMAC/файл-ключ) → волна 7; плиточный редизайн → полировка; полная 9-разделная перестройка админки (7.2) → транш 2 волны 6.

## Волна 6 (меню) — транш 2: выполнен

- Админ-hub: 9 разделов по 7.2 (Состояние, Товары и выгрузка, Экран покупателя, Этикетки, Торговые правила, Оборудование, Сеть и время, Доступ и безопасность, Обслуживание). Вход админа — сразу в hub (stage 11).
- Новые экраны: slint 25 «Экран покупателя» (возврат на главный, название в карточке, кнопка назад), slint 26 «Доступ и безопасность» (пароли, журнал действий), slint 27 «Обслуживание» (отчёты, экспорт, импорт, мастер, перезагрузка). Старый экран Обслуживания (slint 15/stage 14) удалён; «Товары и экран» очищен от покупательских пунктов и экспорта/импорта; дублирующее меню rows-admin в slint 6 удалено.
- «Состояние» возвращается на экран-источник (Kiosk.status_from): кассир — в своё меню, админ — в hub.
- Импорт (stage 15), отчёты (16), журнал (22) перепривязаны к новым родителям.
- ФИКС РЕГРЕССИИ: в 114e199 роль при входе не проставлялась (только сбросы) — Rust-гейт блокировал все админ-пункты у всех; исправлено (k.role при входе), добавлено в тест-поверхность вручную (автотеста на гейт нет — стендовая проверка обязательна).
- Не сделано (транш 3, опционально): привязка групп к отделам в UI (7.3 п.3), статус выгрузки в разделе 2, VNC/SSH-переключатель и автовыход в разделе 8 (волна 7), плиточная сетка кассира.

## Волна 7 (безопасность): выполнена

- S6 (a760808): PBKDF2-HMAC-SHA256 (200k итераций, свои hmac/pbkdf2 на sha2 — без новых крейтов), формат `k2$<iters>$<hash>`, миграция k1$ и голого sha256 при входе (тесты: RFC 4231/векторы PBKDF2, upgrade-тест). БД chmod 0600 на unix. R3: закрыт капом 600с (T5) — Ruling: монотонные часы в памяти дают обход перезапуском, wall-time с капом и sanity-проверкой достаточно.
- S9 (ae1ed01): IP убран с экрана входа (остался в «Состоянии» и «Сеть»).
- S7 (e60e7c5): каталог обмена конфигурацией `/var/lib/openscale/cfg-import` (вне FTP); sanitize-миграция старых конфигов + warning; каталог создаётся при первом обращении.
- S8 (6291b48): пароль imgsync через `--env-password` (не в ps); картинки выгрузки: лимит 4МБ, фильтр `..` и слэшей в имени.
- S11 (aa0990a): run_check.py кладёт скрипт в /root, не /tmp.
- R5 + B12 (cdbd9a9): sudo-хелпер openscale-vnc-toggle (on|off, точные аргументы в sudoers — S10-стиль) + пункт «Удалённый доступ (VNC)» в разделе 8; «Сбросить PIN ключом с USB» — файл openscale-reset.key = hex(sha256("openscale-reset:"+machine-id)), генератор deploy/make_reset_key.py, сброс PIN админа на 12345, запись в журнал.
- S5: закрыт в волне 1 (Task 2 — валидация аргументов settime в самом скрипте). S10: закрыт Ruling'ом волны 1 (sudoers без шаблонов, скрипты валидируют сами; точные аргументы добавлены для новых хелперов).
- S3 (TLS FTP): НЕ делается в этой сессии — требует серверной стороны центра (сертификат на их FTP, порт 990, suppaftp TLS-feature). Задача на согласование с центром; клиентская поддержка добавляется включением feature после готовности сервера.

## Гигиена: выполнена (основная часть)

- Мёртвый код снесён (009e9e3): setup::evaluate, datastore::dept_of/print_log_count_all/print_log_recent (+их тесты), escpos::parse_status/PrinterStatus/EscPosError::Encoding, config.ui.max_cols (+сводка). tick: лишний кортеж ticks/goods упрощён, Mutex → RefCell (tick однопоточный).
- Попапы успех/ошибка различимы цветом (imp-popup-ok/labels-popup-ok теперь читаются: зелёный/красный).
- Календарь един: print::stamp_for(ts) (localtime+UTC-фоллбек) — now_stamp и журнал действий на одной математике.
- MenuPage-компонент: 4 дублирующих меню-блока (hub/торговые правила/доступ/обслуживание) сведены (fe257be).
- to_cp866: № (0xFC) и ° (0xF8) печатаются (было «?»); raster: высота >255 отбраковывается ошибкой (было молчаливое переполнение u8) — тесты (fe257be).

## Гигиена: остаток (осознанно не сделано)

- Три копии PIN-строк, EAN-13 в label-designer (TS) — Rust/TS-дубль трогать не стали: designer — отдельный билд.
- Контрасты ink-faint (#A6ADB8 2:1 на белом) и покупательские формулировки («Закончилась бумага…») — дизайн/копирайт, переехали в docs/ui-style.md для арены прототипов.
- Свайп 60–80px, литералы 992/1008/840px, icons, @tr() — полировка UI, зависит от результатов арены.

