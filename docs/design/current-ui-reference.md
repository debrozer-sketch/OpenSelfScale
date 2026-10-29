# Текущий UI OpenScale — референс для сравнения с превью

Источник: `app/openscale/ui/main.slint` (коммит `ca2cd0c`). Экран 1024×768, шрифт IBM Plex Sans.
Это не рендер превью — это описание того, что реально рисует Slint сейчас.

Общая тема (global Theme):
- фон окна `#F4F4F2` (тёплый нейтральный), карточки `#FFFFFF`, чипы/клавиши `#EEEEEA`, нажатое `#E2E3DF`
- текст `#16181D`, вторичный `#515866`, подписи `#6B7280`
- границы 1px `#E3E4E0`
- акцент синий `#1F5BE0` (только администрирование), успех `#17803D`, опасность `#C62828`, предупреждение `#9A5B00`
- радиусы: панель 20px, карточка 14px, кнопка 12px, внутренний 8px
- шкала шрифта: display 80px (не 92 — лимит femtovg), h1/h2 30px, h3/lg 24px, body 20px, sm 18px, xs 16px
- фото товаров в карточках: `image-fit: contain`, у отделов: `cover`

---

## 2-departments.png → корень каталога, screen 0

Что сейчас рисует Slint:

- Верхняя строка статуса на всю ширину, высота 20px: зелёная/жёлтая/красная плашка, текст 13px.
- Слева крупный вес: `weight-text`, 80px, bold, выравнивание вправо, под ним «кг» 30px; при нестабильном весе цвет янтарный, под цифрами полоска-индикатор 6px.
- Справа логотип (если загружен), высота ~88px, зона удержания 3с — скрытый вход.
- Ниже (y=160) — сетка отделов: плитки DeptTile, ширина колонки 195px, высота 592px, шрифт названия 30px bold. Плитка: скругление 14px, рамка 1px, фон — индивидуальный цвет отдела из БД (`DeptDef.bg/fg`), сверху фото отдела (`cover`, скругление 20px) или светлая заглушка, название внизу по центру, высота текстовой зоны 90px. Свайп влево/вправо по плитке = следующая/предыдущая страница.
- Под сеткой, по центру: пилюля «Поиск товара» 400×56, фон surface, рамка 1px.
- Если страниц отделов больше одной: слева «← НАЗАД» и справа «ДАЛЕЕ →», кнопки 200×56, текст 24px, цвет ink-soft.
- Альтернативный вид (настройка «Вид отделов» = ячейки): плитка-строка с цветной вертикальной полосой 8px слева, название 28px, фон — цвет отдела, разбелённый до 86% белого.

Визуальный код (ключевые фрагменты):

```slint
component DeptTile inherits Rectangle {
    in property <DeptDef> d;
    in property <bool> cell: false;
    border-radius: Theme.r-card;              // 14px
    border-width: 1px;
    border-color: Theme.border;
    background: root.cell
        ? (ta.pressed ? d.bg.mix(#FFFFFF, 78%) : d.bg.mix(#FFFFFF, 86%))
        : (ta.pressed ? d.bg-pressed : d.bg);
    if !root.cell && d.has-img : Rectangle {
        x: 8px; y: 8px;
        width: parent.width - 16px;
        height: parent.height - 110px - 16px;
        border-radius: Theme.r-panel;         // 20px
        clip: true;
        Image { width: 100%; height: 100%; image-fit: cover; source: d.img; }
    }
    Text {
        property <length> ty: root.cell ? 0px : parent.height - 110px;
        x: 8px; y: ty;
        height: root.cell ? parent.height : 90px;
        text: d.name;
        font-size: root.cell ? 28px : root.font;   // root.font = 30px
        font-weight: 700;
        color: d.fg;
        horizontal-alignment: center;
        vertical-alignment: center;
        overflow: elide;
    }
}
```

```slint
// screen 0: сетка отделов
if screen == 0 : Rectangle {
    x: 0; y: 160px;
    width: root.width; height: root.height - 160px;
    for d[i] in root.depts : DeptTile {
        cell: root.dept-cell;
        idx: i;
        col-w: root.col-w;      // 195px
        tile-h: root.tile-h;    // 592px
        font: root.dept-font;   // 30px
        d: d;
        active: root.can-tap;
        tapped(dept) => { root.dept-tapped(dept); }
        next-page => { root.more(); }
        prev-page => { root.back(); }
    }
}
// поиск
if screen == 0 : Rectangle {
    x: root.width / 2 - 200px;
    y: 160px + root.tile-h + 16px;
    width: 400px; height: 56px;
    border-radius: Theme.r-card;
    border-width: 1px; border-color: Theme.border;
    background: search-ta.pressed ? Theme.pressed : Theme.surface;
    Text { text: "Поиск товара"; font-size: Theme.font-lg; color: Theme.ink-faint; vertical-alignment: center; }
    search-ta := TouchArea { clicked => { root.search-open(); } }
}
```

---

## 1-catalog.png → каталог внутри отдела, screen 2

Что сейчас рисует Slint:

- Под статусной строкой (y=160) своя строка-хедер высотой 56px:
  - если кнопка «назад» включена (настройка): одна кнопка «НАЗАД» 110×40 слева;
  - если выключена — горизонтальная лента чипов отделов по 110×40 с шагом 114px, текущий отдел подсвечен `pressed-deep`, по краям маленькие «НАЗАД»/«ДАЛЕЕ» 62×40 для листания ленты.
- Справа в хедере: поле «Поиск товара» 300×40.
- Ниже — сетка товаров PluCard: по умолчанию 9 колонок (настройка «Столбцов с товарами», 6/9/...), ширина карточки 108px, высота 178px, шаг по X 116px, по Y 182px. Карточка: скругление 12px, фон surface (нажатое `pressed`), внутри сверху область изображения 104px высотой на фоне chip с внутренним скруглением 8px и `contain`; имя товара 10px в 2 строки по центру; цена 12px bold; если есть карточная цена — старая цена 9px приглушённо и «по карте» 11px bold зелёным.
- Крайняя карточка «ДАЛЕЕ →» того же размера с рамкой; слева на страницах >1 карточка «← НАЗАД» 108×178.

Визуальный код (ключевые фрагменты):

```slint
component PluCard inherits Rectangle {
    in property <int> cols: 9;
    in property <length> col-w: 108px;
    in property <PluTile> p;
    width: root.col-w;
    height: 178px;
    x: 8px + Math.mod(self.idx + self.cell-offset, self.cols) * (root.col-w + 8px);
    y: 56px + Math.floor((self.idx + self.cell-offset) / self.cols) * 182px;
    border-radius: Theme.r-button;          // 12px
    background: !root.active ? Theme.chip : ta.pressed ? Theme.pressed : Theme.surface;
    Rectangle {                              // фото
        x: 2px; y: root.card-show-name ? 2px : 21px;
        width: parent.width - 4px; height: 104px;
        border-radius: Theme.r-inner;        // 8px
        clip: true;
        background: Theme.chip;
        if root.p.has-img : Image { width: 100%; height: 100%; image-fit: contain; source: root.p.img; }
    }
    if root.card-show-name : Text {          // имя
        x: 3px; y: 108px; width: parent.width - 6px; height: 40px;
        text: root.p.name; font-size: 10px; wrap: word-wrap; overflow: elide;
        horizontal-alignment: center; vertical-alignment: center; color: Theme.ink;
    }
    if root.p.card-price == "" : Text {      // обычная цена
        x: 3px; y: 150px; width: parent.width - 6px; height: 24px;
        text: root.p.price; font-size: 12px; font-weight: 700;
        horizontal-alignment: center; vertical-alignment: center; color: Theme.ink;
    }
    if root.p.card-price != "" : Text {      // старая цена
        x: 3px; y: 147px; width: parent.width - 6px; height: 12px;
        text: root.p.price; font-size: 9px; horizontal-alignment: center; vertical-alignment: center;
        color: Theme.ink-faint;
    }
    if root.p.card-price != "" : Text {      // цена по карте
        x: 3px; y: 158px; width: parent.width - 6px; height: 16px;
        text: root.p.card-price; font-size: 11px; font-weight: 700;
        horizontal-alignment: center; vertical-alignment: center; color: Theme.success-bright;
    }
}
```

Хедер каталога (чипы отделов / кнопка назад + поиск):

```slint
if root.catalog-back-btn : Rectangle {      // «НАЗАД» 110×40
    x: 8px; y: 8px; width: 110px; height: 40px;
    border-radius: Theme.r-button;
    background: cat-back-ta.pressed ? Theme.pressed : Theme.surface;
    Text { text: "НАЗАД"; font-size: Theme.font-xs; font-weight: 700; ... }
}
// иначе: for d[i] in root.header-depts : Rectangle 110×40, шаг 114px,
// активный: background: Theme.pressed-deep
Rectangle {                                  // поиск 300×40 справа
    x: root.width - 308px; y: 8px; width: 300px; height: 40px;
    border-radius: Theme.r-button;
    border-width: 1px; border-color: Theme.border;
    background: search2-ta.pressed ? Theme.pressed : Theme.surface;
    Text { text: "Поиск товара"; font-size: Theme.font-xs; color: Theme.ink-faint; vertical-alignment: center; }
}
```

---

## 3-weighing-sheet.png → подтверждение веса, popup (popup-open)

Что сейчас рисует Slint (не отдельный экран — модальный попап поверх каталога):

- Фон-затемнение: прозрачный кликабельный слой, клик мимо = отмена.
- Панель 720×468 по центру (x=152, y=150), скругление 14px, фон surface, рамка 1px.
- Слева фото товара 280×280, скругление 14px, фон chip, `contain`.
- Справа: имя товара 20px bold (2 строки), ниже «Цена: …» 20px, ниже зелёным bold «По скидочной карте: …» 16px.
- Слева внизу крупно вес: `popup-weight`, 30px bold.
- Справа внизу: «Итого: …» 24px bold и при карточной цене «Со скидочной картой: …» 18px bold зелёным.
- Если вес пуст/меньше минимума вместо веса: «Положите товар на весы» или красное «Вес слишком мал», 24px по центру.
- Внизу две кнопки 200×56: «ОТМЕНА» (surface, рамка, ink-soft) и «ПЕЧАТЬ» (AppButton kind=2, зелёная `#17803D`, белый текст; активна только при стабильном весе и непустом весе).

Визуальный код (ключевые фрагменты):

```slint
if root.popup-open : Rectangle {
    x: 0; y: 0; width: root.width; height: root.height;
    background: transparent;
    popup-back-ta := TouchArea { clicked => { root.plu-cancel(); } }
    Rectangle {
        x: 152px; y: 150px; width: 720px; height: 468px;
        border-radius: Theme.r-card;           // 14px
        border-width: 1px; border-color: Theme.border;
        background: Theme.surface;
        popup-panel-ta := TouchArea { }
        Rectangle {                            // фото 280×280
            x: 24px; y: 24px; width: 280px; height: 280px;
            border-radius: Theme.r-card; clip: true; background: Theme.chip;
            if root.popup-plu.has-img : Image { width: 100%; height: 100%; image-fit: contain; source: root.popup-plu.img; }
        }
        if root.card-show-name : Text {        // имя
            x: 328px; y: 24px; width: 368px; height: 64px;
            text: root.popup-plu.name; font-size: Theme.font-body; font-weight: 700;
            wrap: word-wrap; overflow: elide; vertical-alignment: center;
        }
        Text {                                 // цена
            x: 328px; y: root.card-show-name ? 100px : 24px; width: 368px; height: 32px;
            text: "Цена: " + root.popup-plu.price; font-size: Theme.font-body;
        }
        if root.popup-plu.card-price != "" : Text {
            x: 328px; y: root.card-show-name ? 136px : 60px; width: 368px; height: 32px;
            text: "По скидочной карте: " + root.popup-plu.card-price;
            font-size: Theme.font-md; font-weight: 700; color: Theme.success-bright;
        }
        if !root.popup-empty : Text {          // вес крупно
            x: 24px; y: 312px; width: 300px; height: 72px;
            text: root.popup-weight; font-size: Theme.font-h1; font-weight: 700;
            horizontal-alignment: center; vertical-alignment: center;
        }
        if !root.popup-empty : Text {          // итого
            x: 328px; y: 316px; width: 368px; height: 32px;
            text: "Итого: " + root.popup-sum; font-size: Theme.font-h3; font-weight: 700;
        }
        if root.popup-empty : Text {
            x: 24px; y: 320px; width: 672px; height: 56px;
            text: root.popup-under-min ? "Вес слишком мал" : "Положите товар на весы";
            font-size: Theme.font-lg;
            color: root.popup-under-min ? Theme.danger : Theme.ink-faint;
        }
        Rectangle {                            // отмена
            x: 24px; y: 392px; width: 200px; height: 56px;
            border-radius: Theme.r-card;
            border-width: 1px; border-color: Theme.border;
            background: popup-cancel-ta.pressed ? Theme.pressed : Theme.surface;
            Text { text: "ОТМЕНА"; font-size: Theme.font-body; font-weight: 700; color: Theme.ink-soft; }
        }
        AppButton {                            // печать
            x: 496px; y: 392px; width: 200px; height: 56px;
            kind: 2;                           // success #17803D
            label: "ПЕЧАТЬ"; text-color: Theme.surface;
            enabled: !root.popup-empty && root.weight-state == 2;
            clicked => { root.plu-confirm(); }
        }
    }
}
```

Крупный вес на главном экране (для сравнения цифр):

```slint
Text {
    x: 16px; y: 44px; width: 380px; height: 112px;
    text: root.weight-text;
    font-size: Theme.font-display;             // 80px (лимит femtovg)
    font-weight: 700;
    horizontal-alignment: right; vertical-alignment: center;
    color: weight-state == 0 ? Theme.ink-faint
         : weight-state == 1 ? Theme.warning-deep : Theme.ink;
}
Text { x: 404px; y: 96px; text: "кг"; font-size: Theme.font-h2; color: Theme.ink-faint; }
```

---

## 4-admin-menu.png → меню администратора, screen 12

Что сейчас рисует Slint:

- Экраны ≥4 полностью перекрывают главное окно фоном `Theme.bg`.
- Хедер-карточка на всю ширину минус 8px по бокам, высота 64px, y=8: скругление 14px, surface, рамка 1px. Слева заголовок `admin-title` («МЕНЮ НАСТРОЕК» для этого меню) 24px bold. Справа в хедере кнопки 160×48 на фоне chip: «НАЗАД» (если виден) и «ВЫХОД».
- Ниже (y=84) — список MenuPage: 9 строк-кнопок, каждая 74px высотой с шагом 82px, ширина «вся ширина минус 16px»: скругление 12px, рамка 1px, фон surface, нажатое pressed. Текст 24px bold, отступ слева 24px, без иконок. Строки с `admin: true` для не-админа приглушены до 45% и не кликабельны (в этом меню все 9 — админские).
- Разделы (сверху вниз): Состояние, Товары и выгрузка, Экран покупателя, Этикетки, Торговые правила, Оборудование, Сеть и время, Доступ и безопасность, Обслуживание.

Визуальный код (ключевые фрагменты):

```slint
component MenuPage inherits Rectangle {
    in property <[MenuRow]> rows;
    in property <int> role;
    callback page-tap(string);
    for r[i] in root.rows : Rectangle {
        x: 8px;
        y: 12px + i * 82px;
        width: parent.width - 16px;
        height: 74px;
        border-radius: Theme.r-button;         // 12px
        border-width: 1px;
        border-color: Theme.border;
        background: mp-ta.pressed ? Theme.pressed : Theme.surface;
        opacity: role == 1 || !r.admin ? 1.0 : 0.45;
        Text {
            x: 24px; width: parent.width - 48px; height: parent.height;
            text: r.label + (r.admin && role != 1 ? " · только администратор" : "");
            font-size: Theme.font-h3;          // 24px
            font-weight: 700;
            color: Theme.ink;
            vertical-alignment: center;
            overflow: elide;
        }
        mp-ta := TouchArea {
            clicked => { (role == 1 || !r.admin) ? root.page-tap(r.id) : {}; }
        }
    }
}

if root.screen == 12 : MenuPage {
    x: 0; y: 84px;
    width: parent.width; height: parent.height - 84px;
    rows: [
        { label: "Состояние", id: "status", admin: true },
        { label: "Товары и выгрузка", id: "goods", admin: true },
        { label: "Экран покупателя", id: "custscreen", admin: true },
        { label: "Этикетки", id: "labels", admin: true },
        { label: "Торговые правила", id: "trade", admin: true },
        { label: "Оборудование", id: "equip", admin: true },
        { label: "Сеть и время", id: "net", admin: true },
        { label: "Доступ и безопасность", id: "access", admin: true },
        { label: "Обслуживание", id: "maint", admin: true },
    ];
    role: root.role;
    page-tap(id) => { root.menu-tap(id); }
}
```

Хедер админских экранов:

```slint
if root.screen >= 4 : Rectangle {
    x: 8px; y: 8px; width: parent.width - 16px; height: 64px;
    border-radius: Theme.r-card;
    border-width: 1px; border-color: Theme.border;
    background: Theme.surface;
    Text {
        x: 16px; width: parent.width - 440px; height: parent.height;
        text: root.admin-title;
        font-size: Theme.font-h3; font-weight: 700; color: Theme.ink;
        vertical-alignment: center; overflow: elide;
    }
    if root.admin-back-visible && !root.pin-forced : Rectangle {  // «НАЗАД» 160×48
        x: parent.width - 336px; y: 8px; width: 160px; height: 48px;
        border-radius: Theme.r-button;
        background: admin-back-ta.pressed ? Theme.pressed : Theme.chip;
        Text { text: "НАЗАД"; font-size: Theme.font-lg; font-weight: 700; }
    }
    if root.screen != 19 : Rectangle {                            // «ВЫХОД»/«ГОТОВО» 160×48
        x: parent.width - 176px; y: 8px; width: 160px; height: 48px;
        border-radius: Theme.r-button;
        background: admin-done-ta.pressed ? Theme.pressed : Theme.chip;
        Text { text: root.screen == 4 ? "ГОТОВО" : "ВЫХОД"; font-size: Theme.font-lg; font-weight: 700; }
    }
}
```

---

## Ключевые отличия от превью (ожидаемые, проверить по рендерам)

1. **Колонки каталога**: превью — 6 колонок; текущий дефолт — 9 (`items-cols: 9`, карточка 108×178). 6 колонок выбирается настройкой, но не дефолт.
2. **Плитки отделов**: превью, вероятно, без цветных заливок; текущие плитки красятся индивидуальным цветом отдела из БД и держат фото/название внутри одной высокой плитки 592px.
3. **Меню администратора**: превью — 9 разделов, возможно с иконками; текущее — плоский список без иконок, единый стиль строк 74px.
4. **Подтверждение веса**: превью — отдельный лист; текущее — модальный попап 720×468 поверх каталога, с фото слева и кнопками «ОТМЕНА»/«ПЕЧАТЬ» внизу.
5. **Шрифты**: превью нарисованы «настоящим» Plex в большом размере; в рантайме display-цифры ограничены 80px (femtovg не растеризует ≥88px на GPU киоска).
