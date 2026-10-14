# Журнал работ по аудиту (обновлено 14.10.2026)

Полный список находок — в audit/CODE_AUDIT.md. Здесь — что сделано, что
осталось. Отчёт о состоянии сборки/деплоя — STATUS.md.

## Сделано

### Гигиена и git
- Чистка корня: удалены plink.exe, putty.exe, pscp.exe, reg (ELF старой
  весы), ready.flg, __pycache__.
- reg_item.ps1: пароль удалён, файл перенесён в audit/legacy/reg_item.ps1.txt.
- old.md → audit/old-conveyor.md; var1.md/var2.md → docs/design/;
  plu.xml/layout.xml → testdata/ (в git не попадают).
- .gitignore дополнен: deploy/http/, deploy/netboot/, deploy/musl.tgz,
  deploy/zig.zip, *.deb, vnc_passwd, testdata/, __pycache__/, *.exe, ready.flg.
- git init -b main + первый коммит «Начальное состояние…»; проверено:
  .env, vnc_passwd, exe, testdata в git не попали.

### КРИТ-фиксы (все 6 из аудита)
1. Дата/время на этикетке: build_label(item, weight_g, layout, date, time),
   константы «01.09.26»/«10:31:53» удалены, now_stamp() через
   libc localtime_r (unix) с UTC-фолбэком. Тесты обновлены.
   ⚠ shelflife (срок годности) в кадр пока НЕ выведен — осталось.
2. Пароль в reg_item.ps1 — удалён (см. выше).
3. Гонка многократной печати: print_weight_g u32 → Option<u32>;
   weight_g==0 → откат в фазу 1 (нулевые этикетки невозможны).
4. Фильтр товаров отдела: из БД-карты group_dept (Rc<RefCell<HashMap>>),
   а не из статического сида datastore::dept_of. Коснулся и тапа по
   отделу, и построения плиток.
5. refresh_data: datastore::open и 4 запроса — через match; при ошибке
   log::error и выход, модели НЕ затираются пустотой.
6. git инициализирован (см. выше).

### Баги железа и протоколов
- escpos: enum FieldStyle {Normal, FontB, DoubleH} — байты сняты с живого
  дампа (Normal=ESC!0+GS!0, FontB=ESC!1+GS!0, DoubleH=ESC!0+GS!1; прежний
  «double_height» реально давал 2×2). field() принимает FieldStyle;
  вызовы в print.rs, calib.rs, examples переключены. Отдельно: честный
  EscPosError::InvalidBarcode вместо маскировки под Encoding; fix
  never_loop в status(); комментарий CP1251→CP866.
- weight.rs / drivers-api: добавлен DriverError::Timeout; таймаут опроса
  больше НЕ трактуется как смерть порта — порт жив, stable=false,
  переоткрытия нет.
- shtrih-m: порядок проверок кадра длина→STX→':'→LRC; read_timeout
  реально уважает параметр (unix: libc::poll по fd; хост-цикл с
  дедлайном); libc — target-зависимость для unix.
- sync.rs: остывание картинок по двум замерам размера (mtime ненадёжен);
  ошибки удаления файлов в каталоге приёма логируются (залипший
  ready.flg больше не циклит молча).
- datastore: убрана тавтология в parse_layout_xml (ветка «if это group»
  внутри ветки group).

## НЕ проверено после правок
- cargo check/test после переключения на FieldStyle не прогнаны
  (проверка прервана) — первый шаг при продолжении.
- Сборка zigbuild + деплой на весы + рестарт scale-kiosk не делались:
  на весах работает СТАРАЯ сборка (drop-режим без аудит-фиксов).
- Чекпоинт-коммит по фиксам не сделан.

## Осталось сделать

### Баги/чистка (по приоритету)
- [ ] diag.rs: probe_incoming ищет current/manifest.json мёртвого
  sync-agent — переписать под drop-реальность (каталог, права, последний
  импорт из history).
- [ ] config.rs: дефолт incoming_dir → /var/ftp/scale (сейчас
  /home/scales/incoming — silent-fallback №2); doc-примеры (145-147)
  кладут ключи после [printer] — тот самый TOML-грех.
- [ ] app/kiosk/Cargo.toml: убрать зависимость sync-agent; крейт
  crates/sync-agent удалить или пометить архивом.
- [ ] datastore: удалить groups_by_size (+тест) — ноль вызовов.
- [ ] main.rs: удалить Kiosk.dept_back и ветку в on_up (рудимент);
  parse_cli: expect'ы → печатный usage; usage дополнить
  --sync/--dept-*/--group-assign.
- [ ] main.rs on_more/on_back: с экрана 3 (поиск) попадают в ветку
  отделов и молча выбрасывают на главный — ветвить или скрыть пейджер.
- [ ] main.slint: мёртвый component Tile (36-72); недостижимые ветки
  screen==2 (463, 482-483); property cols не читается (шаг шапки 114px
  размазан по 3 местам); устаревшие комментарии (297, 305); паттерн
  Rectangle+Text+TouchArea ×15 → TextButton (~200 строк экономии).
- [ ] print.rs:95: «PLU не найден» маскируется под QueryReturnedNoRows.
- [ ] ttyprobe.rs: #![cfg(unix)] на весь bin ломает cargo build на
  Windows-хосте (E0601) — заглушка main или required-features.
- [ ] sync.rs: run_drop/run дублируют parse→archive→import→apply_layout
  (~35 строк ×2) → общая apply_upload.
- [ ] datastore: добавить get_item(conn, plu) — схема plucodes
  размазана на два крейта (print.rs маппит вручную).
- [ ] calib.rs: калибровка печати ни на что не влияет — подключить к
  печати или пометить недоделкой в UI.
- [ ] DeptTile не глушится во время печати (can-tap) — проверить
  фильтр в on_dept_tapped.
- [ ] shelflife (срок годности) вывести на этикетку — данные уже
  выбираются в lookup.
- [ ] Мелкое: pub const NAK (shtrih-m) и DriverError::PortOpen не
  используются — удалить или задействовать.

### Документация
- [ ] README/ARCHITECTURE/RECOVERY/ROADMAP описывают sync-agent и
  «никаких флагов» — переписать под drop-протокол с ready.flg.
- [ ] deploy/push.py: docstring говорит /root/install, реально $HOME/install.

### Декомпозиция (после фиксов)
- [ ] main.rs (1451 строк): session.rs (стейт-машина покупки, Kiosk +
  tick — приоритет), depts_cli.rs, tiles.rs, pager.rs (дубли пейджера).

### Финал
- [ ] cargo clippy + test + zigbuild --release.
- [ ] Деплой на весы + рестарт scale-kiosk + проверка живого цикла drop.
- [ ] Коммит-чекпоинт по фиксам.
