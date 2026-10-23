# Старое ПО SelfServiceScales — полный разбор (боевой бэкап, 2026-10-15)

Источник: побайтовый бэкап боевых весов 192.168.41.129 (Ubuntu 16.04) в
`audit/prod-backup/`. Бинарник из бэкапа идентичен ранее изученному
(MD5 071657EB5BA8385B416AC5D7F3BA0ADA) — дизасм в temp/slim/disasm.txt
актуален. Локальная распаковка: temp/slim/prod/.

## 1. Как устроено на боевых

- Автостарт: Upstart-джоб (`~/scales.conf`, start on runlevel 23, respawn) →
  `startx` → `~/.xsessionrc`: xset off/dpms, **x11vnc -forever -passwd REMOVED**,
  `~/start.sh` = бесконечный цикл `./SelfServiceScales` в `~/sss/`.
- Каталог `~/sss`: бинарник (271 КБ, Qt5 QML), `SelfServiceScales.conf`,
  шаблоны `label.xml`/`layout.xml`/`plu.xml`, каталог `img/` (картинки
  товаров по PLU), `history/` (снимки plu+layout при каждой выгрузке),
  БД SQLite `SelfServiceScales.plu`.
- Лог `SelfServiceScales.log` — 1.3 ГБ отладки (формат
  `%1[LEVEL][%3] %2`, dd.MM.yyyy hh:mm:ss.zzz) — чистится вручную
  (`sudo rm`, видно в .bash_history). Урок для нашего kiosk: ротация
  логов обязательна (уже есть, /etc/logrotate.d/openscale).

## 2. Классы и сигналы

- **ShtrihSlim** — весовой модуль `/dev/scales` (ttyACM0, 115200) + мост
  принтера. Сигналы: weightChanged, LabelIsPrinted, labelIsTakenOff,
  showMessage, criticalError, printerPortError. Слоты: updateWeight,
  checkLabelState, feedLabel, testPrint, loadLogos, printPLU, setZero,
  sendOptoThermoSpace, sendBackwardSpace, start/stopCheckLabelState,
  startMonitoring.
- **Printer** — тонкая обёртка ESC/POS (полная карта команд — PROTOCOL.md §4).
- **Database** — SQLite `SelfServiceScales.plu`, таблица plucodes;
  миграции ALTER TABLE (description, shelflife, actionprice);
  PRAGMA journal_mode=MEMORY, page_size=4096, cache_size=16384,
  temp_store=MEMORY. SQL: SELECT/INSERT OR REPLACE по 9 полям.
- **DataUpdater** — выгрузка: `/var/ftp/scale/ready.flg` → plu.xml +
  layout.xml + img/*.jpeg → БД + копирование картинок; подтверждение
  `uploading.flg`; снимки в `history/yyyy-MM-dd hh-mm-ss/`.
  Сеть: libQt5Network, getIPAddress (IP для показа на экране?).
- **ScalesStub** — заглушка без железа (наш аналог хост-сборки).

## 3. Статус весов: два байта, 8 флагов

Старый драйвер берёт из кадра ответа `mid(5,2)` — ДВА байта статуса —
и разворачивает биты 0..6 в отдельные поля ScalesState (дизасм 40ad3f–40ad9b,
последовательность `shr $n; and $0x1`). Наши замеры дают:
`0x15 0x00` стабильно, `0x14 0x00` пусто, `0x04` движение, `0x16 0x00` пусто.
Бит0 = «стабильно». Биты 2 и 4 наблюдаются всегда, бит1 — в варианте «пусто».
Второй статусный байт — всегда 0x00 на живых замерах.

**Термобашня/зуммер — финальный вывод:** датчик есть (прошивка модуля
пищит при открытии на боевых), но ни один из 9 протокольных каналов
(кадры веса с обоими статусными байтами, DLE EOT 1–7, GS r 1/49/65,
ESC u/v, DLE ENQ, unsolicited) не меняется при открытии/закрытии.
Старый драйвер зуммером не управлял (нет ни одной конфигурационной
команды в ShtrihSlim). Писк — чисто аппаратная фича прошивки модуля;
на тестовом модуле после замены образа писк пропал — прошивка модулей
различается. Программно автокалибровку по башне не построить; решение —
ручная подача из меню. Настройки зуммера в сервис-доках (repair.txt,
operator.txt) нет.

## 4. Экраны и тексты старого ПО (полный список)

«Ошибка весовой платформы», «Устройство не найдено», «Закончилась бумага
в принтере», «отпечатанная этикетка снята», «Нет информации о товаре»,
«Вес нестабилен», «Спасибо за покупку», «Версия» (ПО/прошивка),
«Тестовая печать». Тест-текст шрифтов — джабервокки «Варкалось. Хлтвкие
шорьки пырялись по наве…» (все глифы CP866). QML-ресурсы вшиты в бинарник
(qrc): images/next.png, prev.png, default.png (заглушка фото товара при
Image.Error), кнопки с долгим нажатием (Timer 50 мс), плитка товара:
фото в круг-маске (OpacityMask), до 4 строк текста, серый разделитель.

## 5. Конфиг-ключи старого ПО (полный набор)

`common/logLevel`, `common/idleInterval`, `scales/port`,
`scales/additionalLabelFeed`, `scales/additionalFeedAmount`,
`scales/labelFormat`, `printer/port`, `printer/optoThermoSpace`,
`printer/backwardSpace`, `printer/disableStatusCheck`.

Боевые значения: optoThermoSpace=375, backwardSpace=80,
additionalLabelFeed=0, labelFormat=1, disableStatusCheck=1 (!),
ports: /dev/scales, /dev/ttyS3. Резервная копия конфига второй машины
(conf.Lh2685): 382/78. Вывод прежний: калибровки индивидуальны,
хранятся на устройстве.

disableStatusCheck=1 на боевых — статус принтера выключен (причина
проблемы «не видит конец бумаги», PROBLEMS А1). Наше ПО статусы
опрашивает всегда — осознанное отличие.

## 6. Шаблон этикетки боевых (label.xml, живой прод!)

Ширина 480 точек, высота до ~440: namefirst (y40), namesecond (y64),
description (y88), barcode EAN-13 (x10 y304 h48), date (x208 y265),
time (x208 y282), shelflife (x290 y265), weight (x380 y265), пары
«По акции/Без акции» (actionprice/price/actionsum/sum, y330–360,
с атрибутами noaction/action — поля прячутся по режиму), статиктексты
«Упаковщик: оператор #1», «ООО "AAA"», «Факт.: г.Москва, Кремль».
Элементы: field/statictext/barcode/rectangle/EmbeddedImage (лого в
NV-памяти принтера, FS q грузится loadLogos при старте из qrc-PNG).

Это эталон для редактора этикеток и для LIVE_LAYOUT нового ПО: боевые
этикетки заметно отличаются от дампа 172.32.0.5 (448×232). Шаблон должен
приезжать с центра — подтверждено прод-данными.

## 7. Что нового ПО ещё не делает (сводная, см. ROADMAP §1)

1. Лого магазина на этикетке (FS q + EmbeddedImage).
2. DLE ENQ после заправки бумаги (выход из error).
3. GS ( A — сервисный тест принтера (head/датчики).
4. Ожидание снятия этикетки (DLE EOT по таймеру 0.5 с) вместо
   фиксированного таймаута.
5. ESC J n (additionalLabelFeed), RS G (backwardSpace) — применение
   калибровки в потоке печати (сейчас — только RS F из CLI).
6. Автоноль/«Ошибкa весовой платформы» как отдельные состояния экрана —
   частично покрыто статус-полосой.
7. Экран «Версия» — версия ПО/прошивки из меню (у нас есть в диагностике).

## 8. Не перенесено сознательно

- x11vnc с фиксированным паролем `scale` (небезопасно, конфликтует с нашей
  схемой панель+VNC).
- Цикл `while [ 1 ]; do ./SelfServiceScales; done` — вместо systemd
  Restart=always.
- disableStatusCheck=1 (не видит конец бумаги — баг, не фича).
- Лог без ротации (1.3 ГБ).
