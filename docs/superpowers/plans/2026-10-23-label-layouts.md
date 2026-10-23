# Динамические раскладки этикеток — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Печать по загружаемым раскладкам этикеток: реестр в БД, импорт
JSON из каталога обмена, экран «Этикетки» на весах, автоподбор по замеренной
длине рулона, конструктор на ПК в формате спеки.

**Architecture:** Новый модуль `app/kiosk/src/label.rs` — модель раскладки,
парсер/валидатор JSON, подбор по длине. Реестр — таблица `label_layouts`
в `datastore` (INSERT OR REPLACE по имени). `print.rs::build_label`
переводит мм → доты (константа `MM_TO_DOTS: i32 = 8`, 203 dpi) и рендерит
через существующий `escpos::LabelJob`; пустой реестр = текущее поведение
(`LIVE_LAYOUT`). Замер длины уже есть (`print.rs::scan_label_length`,
ключ `label_length_mm` в `calibration`). Экран 20 в main.slint по образцу
экрана импорта конфига (16). Комментарии в коде не писать (правило проекта).

**Tech Stack:** Rust (rusqlite, serde_json уже в дереве зависимостей),
Slint, vanilla HTML/JS в конструкторе.

**Spec:** `docs/superpowers/specs/2026-10-19-label-designer-design.md`

## Global Constraints

- Программа не зависает: UI-поток без принтера/БД-блокировок; замер длины —
  в фоновом потоке с дедлайном (образец — существующий поток калибровки
  шага 5 мастера, guard по номеру шага).
- Печать никогда не блокируется: невалидная раскладка/нет данных — печать
  продолжается (пропуск поля, не ошибка печати).
- Единицы раскладки — миллиметры; перевод в доты только через константу
  `MM_TO_DOTS = 8` в одном месте.
- Текст — CP866 (текущее `to_cp866`), неизвестные символы → `?`.
- `cardprice` без скидочной цены — пустое место, не ошибка.
- Имя файла раскладки = `name` из JSON; расширение `.json`, префикс `label-`.
- Комментарии в коде запрещены.

## Review Focus

- JSON с неизвестным kind — импорт отклоняется целиком, реестр не меняется
  (тест в Task 2).
- Раскладка с полем за границей листа — отклонение при импорте (тест Task 2).
- Две раскладки с одинаковой длиной — детерминированный выбор ближайшей
  (тест Task 5).
- Нет замера длины (`label_length_mm` отсутствует) — печать по стоку,
  продажа не блокируется (тест Task 5).
- `text` длиннее 48 символов и пустой — отклонение импорта (тест Task 2).

---

### Task 1: Реестр раскладок в datastore

**Files:**
- Modify: `crates/datastore/src/lib.rs` (ensure_tables + функции)

**Interfaces:**
- Produces:
  - `pub struct LabelLayoutRow { pub name: String, pub width_mm: i32, pub height_mm: i32, pub json: String }`
  - `pub fn label_layouts_list(conn) -> Result<Vec<LabelLayoutRow>, StoreError>`
  - `pub fn label_layout_get(conn, name: &str) -> Result<Option<LabelLayoutRow>, StoreError>`
  - `pub fn label_layout_put(conn, row: &LabelLayoutRow) -> Result<(), StoreError>`
  - `pub fn label_layout_del(conn, name: &str) -> Result<(), StoreError>`

- [ ] **Step 1:** Тест (в `crates/datastore/src/lib.rs` tests): put → list
      (1 запись, поля совпадают), put того же имени → replace (list = 1),
      get отсутствующего → None, del → get None.
- [ ] **Step 2:** Прогон: `cargo test -p datastore --target x86_64-pc-windows-msvc` — FAIL (нет таблицы/функций).
- [ ] **Step 3:** Реализация: `CREATE TABLE IF NOT EXISTS label_layouts
      (name TEXT PRIMARY KEY, width_mm INTEGER, height_mm INTEGER, json TEXT NOT NULL)`,
      функции по сигнатурам.
- [ ] **Step 4:** Прогон — PASS.
- [ ] **Step 5:** Commit: `datastore: реестр раскладок этикеток (label_layouts)`.

### Task 2: Модель и парсер раскладки (label.rs)

**Files:**
- Create: `app/kiosk/src/label.rs`; зарегистрировать `mod label;` в `main.rs`.

**Interfaces:**
- Consumes: ничего.
- Produces:
  - `pub const MM_TO_DOTS: i32 = 8;`
  - `#[derive(Clone, Copy, PartialEq, Debug)] pub enum LabelKind { Name, Plu, Barcode, Weight, Price, Sum, Date, Time, Shelflife, Cardprice, Text }` + `parse_kind(&str) -> Option<LabelKind>`, `kind_name(self) -> &'static str`.
  - `#[derive(Clone, Debug)] pub struct LabelField { pub kind: LabelKind, pub x_mm: i32, pub y_mm: i32, pub font_a: bool, pub size2: bool, pub text: String }`
  - `#[derive(Clone, Debug)] pub struct LabelSpec { pub name: String, pub width_mm: i32, pub height_mm: i32, pub fields: Vec<LabelField> }`
  - `pub fn parse(spec_json: &str) -> Result<LabelSpec, String>` — JSON по спеке §1;
  - `pub fn choose(specs: &[LabelSpec], len_mm: i32) -> Option<LabelSpec>`
    (|длина − height_mm| ≤ 5 → ближайшая по модулю; детерминированно: при
    равенстве — первая по имени).

- [ ] **Step 1:** Тесты в `label.rs`:
      валидный минимальный JSON (name/weight/sum/barcode + text) парсится;
      `format != 1` → Err; отсутствие обязательного kind → Err; неизвестный
      kind → Err; text пустой и >48 символов → Err; x/y вне листа → Err;
      name из JSON ≠ имени не проверяется здесь; `choose`: попадание ±5,
      вне допуска → None, две в допуске → ближайшая.
- [ ] **Step 2:** `cargo test -p kiosk label --target x86_64-pc-windows-msvc` — FAIL.
- [ ] **Step 3:** Реализация парсера (serde_json → промежуточные структуры
      с `#[serde(deny_unknown_fields)]` на поле kind недопустимо — kind
      разбирается вручную для честной ошибки «неизвестный kind»).
- [ ] **Step 4:** Прогон — PASS.
- [ ] **Step 5:** Commit: `label: модель, парсер и валидатор JSON-раскладок`.

### Task 3: escpos — стиль «двойная высота и ширина» (size 2)

**Files:**
- Modify: `crates/escpos/src/lib.rs` (FieldStyle, field()).

**Interfaces:**
- Produces: `FieldStyle::Dbl` — ESC ! 0x20 / GS ! 0x11 (2×2).

- [ ] **Step 1:** Тест: `field()` с `FieldStyle::Dbl` содержит байты 0x1B 0x21 0x20 и 0x1D 0x21 0x11.
- [ ] **Step 2:** Прогон `cargo test -p escpos --target x86_64-pc-windows-msvc` — FAIL.
- [ ] **Step 3:** Вариант + байты в `field()`.
- [ ] **Step 4:** PASS. **Step 5:** Commit: `escpos: стиль Dbl (2x2) для size 2`.

### Task 4: build_label от раскладки

**Files:**
- Modify: `app/kiosk/src/print.rs` (build_label → build_label_from_spec,
  значения полей; старый путь — fallback).

**Interfaces:**
- Consumes: `LabelSpec` (Task 2), `LabelJob` + `FieldStyle::Dbl` (Task 3),
  `MM_TO_DOTS`.
- Produces: `fn build_label(item: &PluItem, weight_g: u32, sum: u64, spec: Option<&LabelSpec>) -> Result<Vec<u8>, PrintError>`
  (None → текущий `LIVE_LAYOUT`-путь без изменений); значения полей:
  name=namefirst, plu=`PLU <itemcode>`, barcode=`ean13_payload`, weight
  (`1,350`), price, sum (DoubleH эквивалент = size2 при size2 в поле,
  округление суммы через `round_sum`), date/time=`now_stamp()`,
  shelflife=`shelflife_label`, cardprice=`card_price` (пусто, если нет),
  text=текст из раскладки. Область `ESC W` = width_mm×MM_TO_DOTS ×
  height_mm×MM_TO_DOTS.

- [ ] **Step 1:** Тест: раскладка 58×40 с полями name(2,1,A,1) и
      text(10,36,B,1) «СПАСИБО» → байты содержат ESC W с 464/320 дот,
      `ESC $` x=16, `GS $` y=8, текст namefirst и «СПАСИБО» в CP866;
      cardprice при пустой card_price — поля нет; неизвестный вариант
      невозможен (enum). Fallback: spec=None → байты совпадают со старым
      `build_label` (текущий тест печати остаётся зелёным).
- [ ] **Step 2:** Прогон — FAIL.
- [ ] **Step 3:** Реализация.
- [ ] **Step 4:** PASS. **Step 5:** Commit: `print: рендер этикетки по JSON-раскладке`.

### Task 5: Выбор раскладки при печати

**Files:**
- Modify: `app/kiosk/src/print.rs` (print_for_plu), `app/kiosk/src/label.rs` (грузчик).

**Interfaces:**
- Consumes: `label_layouts_list` (Task 1), `label::parse` + `choose` (Task 2),
  `get_calibration("label_length_mm")`, `build_label` (Task 4).
- Produces: `pub fn pick_layout(conn, measured_len_mm: Option<i32>) -> Option<LabelSpec>`
  (нет замера или реестр пуст/битый JSON записи → None → сток; лог
  warning на битую запись).

- [ ] **Step 1:** Тесты `label.rs`: pick — пустой реестр → None; замер None
      → None; запись с кривым JSON пропускается, остальные работают;
      подходит по длине → выбрана.
- [ ] **Step 2:** FAIL. **Step 3:** Реализация + интеграция в `print_for_plu`
      (один вызов `pick_layout`, `build_label(..., spec.as_ref())`).
- [ ] **Step 4:** PASS (все тесты печати зелёные). **Step 5:** Commit:
      `print: автоподбор раскладки по замеренной длине этикетки`.

### Task 6: Экран «Этикетки» (экран 20) и импорт

**Files:**
- Modify: `app/kiosk/ui/main.slint` (блок `screen == 20`, пункт «Этикетки»
  в rows экрана 9, заголовок в тернарнике), `app/kiosk/src/settings.rs`
  (menu-tap `labels`, загрузка списка, импорт `label-*.json` из
  `[kiosk] cfg_export_dir`, удаление, «Проверить этикетку» — фоновый
  замер + попап результата: измерено N мм, раскладка M/сток),
  `app/kiosk/src/tick.rs` (маппинг admin_stage → screen 20).

**Interfaces:**
- Consumes: Tasks 1–2 (`label_layout_*`, `label::parse`, `choose`),
  `scan_label_length` (существующий), паттерн импорта экрана 16
  (imp-files/preview/apply) — но без подтверждения: список файлов
  `label-*.json`, тап = импорт (INSERT OR REPLACE), попап итог/ошибка.
- Produces: UI-свойства `labels-rows` (имя, WxH, полей), `labels-status`.

- [ ] **Step 1:** Тест на выборку файлов: функция `label_json_files(dir) ->
      Vec<PathBuf>` возвращает только `label-*.json` (тест во временной
      папке: label-a.json, other.json, label-b.txt).
- [ ] **Step 2:** FAIL. **Step 3:** Реализация + UI (список 4 строк max
      со скроллом не нужен — лимит 8 раскладок при импорте: 9-й файл
      отклоняется с попапом), «Проверить этикетку» по образцу потока
      калибровки мастера (дедлайн, invoke_from_event_loop).
- [ ] **Step 4:** Тесты + полный `cargo test` PASS.
- [ ] **Step 5:** Деплой на стенд, ручная проверка экрана. Commit:
      `ui: экран «Этикетки» — реестр, импорт label-*.json, проверка рулона`.

### Task 7: Конструктор HTML в формате спеки

**Files:**
- Modify: `tools/label-designer.html`.

**Interfaces:**
- Produces: JSON точно формата §1 (format/name/width_mm/height_mm/fields
  с kind, x, y, font, size, text), файл скачивается как
  `label-<name>.json`; поля-образцы данными (название товара, «1,350 кг»,
  цены, «Годен до ДД.ММ.ГГГГ»); пресеты размеров: узкая/средняя/квадрат/
  длинная (значения в мм вводимые); валидация перед скачиванием: те же
  правила, что в Task 2 (обязательные kind, границы, text ≤ 48).

- [ ] **Step 1:** Правка HTML: сетка в мм, drag-and-drop, инспектор поля
      (kind/font/size/x/y/text), кнопки «Создать/Открыть/Скачать»,
      валидация.
- [ ] **Step 2:** Ручная проверка в браузере: открыть JSON из Task 2
      теста, пересохранить, скачанный файл проходит `label::parse`
      (юзер прогоняет через импорт на весах).
- [ ] **Step 3:** Commit: `tools: конструктор этикеток в формате спеки (мм, kind, A/B, size)`.

### Task 8: Стенд-прогон и финализация

- [ ] Замер `label_length_mm` на текущем рулоне («Проверить этикетку»).
- [ ] Сборка раскладки текущего рулона в конструкторе, импорт на весах,
      боевая печать — позиция совпадает со стоком или корректна.
- [ ] Удаление раскладки → печать по стоку.
- [ ] FIXLOG/PROTOCOL: результаты замеров (периоды шагов по размерам),
      скриншоты форматов JSON.
- [ ] Пересборка дистрибутивов `python dist/make_distro.py`, коммит,
      пуш.
