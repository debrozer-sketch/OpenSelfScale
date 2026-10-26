import { useEffect, useMemo, useRef, useState } from "react";

type Kind = "name" | "plu" | "barcode" | "weight" | "price" | "sum" | "date" | "time" | "shelflife" | "cardprice" | "text";
type LabelField = { id: string; kind: Kind; x: number; y: number; font: "A" | "B"; size: 1 | 2; text?: string; w?: number; h?: number };
type Layout = { format: 1; name: string; width_mm: number; height_mm: number; fields: LabelField[] };

const LABELS: Record<Kind, string> = {
  name: "Название товара", plu: "Код PLU", barcode: "Штрихкод", weight: "Вес", price: "Цена",
  sum: "Сумма", date: "Дата", time: "Время", shelflife: "Срок годности", cardprice: "Цена по карте", text: "Произвольный текст",
};
const SAMPLES: Record<Kind, string> = {
  name: "ВИНОГРАД КИШМИШ ЧЕРНЫЙ КГ", plu: "PLU 2502192", barcode: "210213601350", weight: "1,350 кг",
  price: "199,99 ₽/кг", sum: "269,99 ₽", date: "19.10.26", time: "18:40:00", shelflife: "Годен до 20.09.2026",
  cardprice: "Со скидкой: 249,99 ₽", text: "ТЕКСТ",
};
const KINDS = Object.keys(LABELS) as Kind[];
const REQUIRED: Kind[] = ["name", "weight", "sum", "barcode"];
const PRESETS = [{ name: "Длинная", w: 58, h: 40 }, { name: "Средняя", w: 58, h: 30 }, { name: "Узкая", w: 58, h: 20 }, { name: "Квадрат", w: 40, h: 40 }];

const uid = () => Math.random().toString(36).slice(2, 9);
const snap = (n: number) => Math.round(n * 2) / 2;
const makeField = (kind: Kind, x = 2, y = 2): LabelField => ({ id: uid(), kind, x, y, font: kind === "name" ? "B" : "A", size: 1, ...(kind === "text" ? { text: "ТЕКСТ" } : {}) });
const initialLayout = (): Layout => ({
  format: 1, name: "58x40", width_mm: 58, height_mm: 40,
  fields: [makeField("name", 2, 1), makeField("barcode", 2, 5), makeField("weight", 2, 21), makeField("price", 2, 26), makeField("sum", 2, 31)],
});

function fieldSize(field: LabelField) {
  if (field.w && field.h) return { w: field.w, h: field.h };
  if (field.kind === "barcode") return { w: field.w || 36 * field.size, h: field.h || 14 * field.size };
  const cw = field.font === "A" ? 3 : 2;
  const ch = field.font === "A" ? 4 : 3;
  const multiplier = field.kind === "sum" ? field.size * 2 : field.size;
  const text = field.kind === "text" ? field.text || "" : SAMPLES[field.kind];
  return { w: field.w || Math.max(2, text.length * cw * multiplier), h: field.h || ch * multiplier };
}

function Icon({ name, size = 16 }: { name: string; size?: number }) {
  const paths: Record<string, React.ReactNode> = {
    undo: <><path d="M9 14 4 9l5-5"/><path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11"/></>,
    download: <><path d="M12 3v12"/><path d="m7 10 5 5 5-5"/><path d="M5 21h14"/></>, plus: <><path d="M12 5v14"/><path d="M5 12h14"/></>,
    copy: <><rect x="8" y="8" width="11" height="11" rx="2"/><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"/></>,
    trash: <><path d="M4 7h16"/><path d="M9 7V4h6v3"/><path d="m7 7 1 14h8l1-14"/></>, chevron: <path d="m9 18 6-6-6-6"/>,
    grip: <><circle cx="9" cy="7" r="1" fill="currentColor" stroke="none"/><circle cx="15" cy="7" r="1" fill="currentColor" stroke="none"/><circle cx="9" cy="12" r="1" fill="currentColor" stroke="none"/><circle cx="15" cy="12" r="1" fill="currentColor" stroke="none"/><circle cx="9" cy="17" r="1" fill="currentColor" stroke="none"/><circle cx="15" cy="17" r="1" fill="currentColor" stroke="none"/></>,
    layers: <><path d="m12 3-9 5 9 5 9-5-9-5Z"/><path d="m3 12 9 5 9-5"/><path d="m3 16 9 5 9-5"/></>,
    sliders: <><path d="M4 6h10"/><path d="M18 6h2"/><circle cx="16" cy="6" r="2"/><path d="M4 12h2"/><path d="M10 12h10"/><circle cx="8" cy="12" r="2"/><path d="M4 18h8"/><path d="M16 18h4"/><circle cx="14" cy="18" r="2"/></>,
    warning: <><path d="M12 3 2.5 20h19L12 3Z"/><path d="M12 9v5"/><path d="M12 17h.01"/></>,
  };
  return <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{paths[name]}</svg>;
}

function NumberField({ label, value, onChange, min = 0, max = 200 }: { label: string; value: number; onChange: (v: number) => void; min?: number; max?: number }) {
  return <label className="field-control"><span>{label}</span><input type="number" value={value} min={min} max={max} step="0.5" onChange={e => onChange(Number(e.target.value))} /></label>;
}

export default function App() {
  const [layout, setLayout] = useState<Layout>(() => {
    try { const saved = localStorage.getItem("openscale.labeldesigner.draft.v2"); return saved ? JSON.parse(saved) : initialLayout(); } catch { return initialLayout(); }
  });
  const [selectedId, setSelectedId] = useState<string | null>(layout.fields[0]?.id || null);
  const [history, setHistory] = useState<Layout[]>([]);
  const [fileOpen, setFileOpen] = useState(false);
  const [addOpen, setAddOpen] = useState(false);
  const [view, setView] = useState<"edit" | "print">("edit");
  const [zoom, setZoom] = useState<"auto" | 4 | 6 | 8>("auto");
  const [status, setStatus] = useState("Черновик сохранен");
  const [mobilePanel, setMobilePanel] = useState<"structure" | "preview" | "properties">("preview");
  const fileRef = useRef<HTMLInputElement>(null);
  const dragRef = useRef<{ id: string; x: number; y: number; startX: number; startY: number; before: Layout } | null>(null);

  const selected = layout.fields.find(f => f.id === selectedId) || null;
  const issues = useMemo(() => {
    const result: string[] = [];
    if (!layout.name.trim()) result.push("Не указано имя раскладки");
    REQUIRED.forEach(k => { if (!layout.fields.some(f => f.kind === k)) result.push(`Нет обязательного поля: ${LABELS[k]}`); });
    layout.fields.forEach(f => { const s = fieldSize(f); if (f.x < 0 || f.y < 0 || f.x + s.w > layout.width_mm || f.y + s.h > layout.height_mm) result.push(`${LABELS[f.kind]} выходит за границы`); });
    return result;
  }, [layout]);
  const missing = REQUIRED.filter(k => !layout.fields.some(f => f.kind === k));

  useEffect(() => { localStorage.setItem("openscale.labeldesigner.draft.v2", JSON.stringify(layout)); }, [layout]);
  const commit = (next: Layout, message = "Изменения сохранены") => { setHistory(h => [...h.slice(-49), layout]); setLayout(next); setStatus(message); };
  const patchSelected = (patch: Partial<LabelField>) => { if (selected) commit({ ...layout, fields: layout.fields.map(f => f.id === selected.id ? { ...f, ...patch } : f) }); };
  const undo = () => { const previous = history.at(-1); if (!previous) return; setLayout(previous); setHistory(h => h.slice(0, -1)); setStatus("Последнее действие отменено"); };
  const addField = (kind: Kind) => { const f = makeField(kind); commit({ ...layout, fields: [...layout.fields, f] }, `Добавлено: ${LABELS[kind]}`); setSelectedId(f.id); setAddOpen(false); setMobilePanel("properties"); };
  const removeSelected = () => { if (!selected) return; const index = layout.fields.findIndex(f => f.id === selected.id); const next = layout.fields.filter(f => f.id !== selected.id); commit({ ...layout, fields: next }, `Удалено: ${LABELS[selected.kind]}`); setSelectedId(next[Math.min(index, next.length - 1)]?.id || null); };
  const duplicateSelected = () => { if (!selected) return; const copy = { ...selected, id: uid(), x: snap(selected.x + 2), y: snap(selected.y + 2) }; commit({ ...layout, fields: [...layout.fields, copy] }, "Создана копия поля"); setSelectedId(copy.id); };
  const reorder = (index: number, dir: -1 | 1) => { const to = index + dir; if (to < 0 || to >= layout.fields.length) return; const fields = [...layout.fields]; [fields[index], fields[to]] = [fields[to], fields[index]]; commit({ ...layout, fields }, "Порядок печати изменен"); };
  const download = () => {
    if (issues.length) { setStatus(`Исправьте замечания: ${issues.length}`); return; }
    const output = { ...layout, fields: layout.fields.map(({ id: _id, ...f }) => f) };
    const url = URL.createObjectURL(new Blob([JSON.stringify(output, null, 2)], { type: "application/json" }));
    const a = document.createElement("a"); a.href = url; a.download = `label-${layout.name.replace(/\s+/g, "_")}.json`; a.click(); URL.revokeObjectURL(url); setStatus("JSON скачан");
  };
  const importFile = async (file?: File) => { if (!file) return; try { const raw = JSON.parse(await file.text()); if (raw.format !== 1 || !Array.isArray(raw.fields)) throw new Error(); const next: Layout = { ...raw, fields: raw.fields.map((f: LabelField) => ({ ...f, id: uid() })) }; commit(next, "Раскладка загружена"); setSelectedId(next.fields[0]?.id || null); } catch { setStatus("Не удалось открыть JSON"); } };
  const newLayout = () => { const next = initialLayout(); commit(next, "Создана новая раскладка"); setSelectedId(next.fields[0].id); setFileOpen(false); };

  const canvasScale = zoom === "auto" ? 6 : zoom;
  const pointerDown = (e: React.PointerEvent, f: LabelField) => { e.preventDefault(); e.currentTarget.setPointerCapture(e.pointerId); setSelectedId(f.id); setMobilePanel("properties"); dragRef.current = { id: f.id, x: f.x, y: f.y, startX: e.clientX, startY: e.clientY, before: layout }; };
  const pointerMove = (e: React.PointerEvent) => { const d = dragRef.current; if (!d) return; const nextX = snap(d.x + (e.clientX - d.startX) / canvasScale); const nextY = snap(d.y + (e.clientY - d.startY) / canvasScale); setLayout(l => ({ ...l, fields: l.fields.map(f => f.id === d.id ? { ...f, x: nextX, y: nextY } : f) })); };
  const pointerUp = () => { const d = dragRef.current; if (d) { setHistory(h => [...h.slice(-49), d.before]); setStatus("Положение поля изменено"); dragRef.current = null; } };

  return (
    <div className="app-shell">
      <header className="topbar">
        <div className="topbar-group"><div className="brand">OpenScale <span>этикетки</span></div><div className="menu-wrap"><button className="button" aria-expanded={fileOpen} onClick={() => setFileOpen(v => !v)}>Файл <span className="caret" /></button>{fileOpen && <div className="file-menu" role="menu"><button role="menuitem" onClick={newLayout}>Создать с нуля</button><button role="menuitem" onClick={() => fileRef.current?.click()}>Открыть файл JSON...</button><button role="menuitem" onClick={() => navigator.clipboard.writeText(JSON.stringify(layout, null, 2)).then(() => setStatus("JSON скопирован"))}>Копировать JSON</button><div className="menu-separator" /><button role="menuitem" onClick={() => setStatus("Выберите объект слева или на макете, затем измените его свойства справа")}>Справка</button></div>}</div><button className="button ghost" onClick={undo} disabled={!history.length}><Icon name="undo" /> Отменить</button></div>
        <div className="document-title"><span className={issues.length ? "status-dot bad" : "status-dot"} />{layout.name || "Без имени"}</div>
        <div className="topbar-group"><span className={issues.length ? "validation bad" : "validation"}>{issues.length ? `${issues.length} замечаний` : "Готово к печати"}</span><button className="button primary" onClick={download}><Icon name="download" /> Скачать JSON</button></div>
      </header>
      <nav className="mobile-nav" aria-label="Области редактора">{(["structure", "preview", "properties"] as const).map((key, i) => <button key={key} aria-pressed={mobilePanel === key} onClick={() => setMobilePanel(key)}>{["Структура", "Превью", "Свойства"][i]}</button>)}</nav>
      <main className="workspace">
        <aside className={`left-panel ${mobilePanel === "structure" ? "mobile-visible" : ""}`} aria-label="Структура этикетки">
          <section className="panel-section document-section"><div className="section-title"><span>Документ</span><span className="measure">мм</span></div><label className="field-control"><span>Имя раскладки</span><input value={layout.name} onChange={e => commit({ ...layout, name: e.target.value })} /></label><div className="field-grid"><NumberField label="Ширина" value={layout.width_mm} min={20} max={80} onChange={v => commit({ ...layout, width_mm: v })} /><NumberField label="Высота" value={layout.height_mm} min={20} onChange={v => commit({ ...layout, height_mm: v })} /></div><div className="preset-row" aria-label="Стандартные форматы">{PRESETS.map(p => <button key={p.name} className={p.w === layout.width_mm && p.h === layout.height_mm ? "active" : ""} title={`${p.name}: ${p.w} x ${p.h} мм`} onClick={() => commit({ ...layout, width_mm: p.w, height_mm: p.h, name: `${p.w}x${p.h}` })}>{p.w}x{p.h}</button>)}</div></section>
          <section className="layers-section"><div className="layers-head"><div><h2><Icon name="layers" /> Объекты</h2><p>Порядок печати сверху вниз</p></div><div className="add-wrap"><button className="icon-button" aria-label="Добавить поле" aria-expanded={addOpen} onClick={() => setAddOpen(v => !v)}><Icon name="plus" /></button>{addOpen && <div className="add-menu">{KINDS.map(k => <button key={k} onClick={() => addField(k)}><span>{LABELS[k]}</span>{layout.fields.some(f => f.kind === k) && <small>есть</small>}</button>)}</div>}</div></div>
            {missing.length > 0 && <div className="required-note"><Icon name="warning" /><div><strong>Не хватает обязательных полей</strong><div>{missing.map(k => <button key={k} onClick={() => addField(k)}>+ {LABELS[k]}</button>)}</div></div></div>}
            <ol className="layer-list">{layout.fields.map((f, index) => { const s = fieldSize(f); const bad = f.x < 0 || f.y < 0 || f.x + s.w > layout.width_mm || f.y + s.h > layout.height_mm; return <li key={f.id} className={selectedId === f.id ? "selected" : ""}><span className="grip"><Icon name="grip" /></span><button className="layer-main" onClick={() => { setSelectedId(f.id); setMobilePanel("properties"); }}><span className="layer-index">{index + 1}</span><span><strong>{LABELS[f.kind]}</strong><small>Шрифт {f.font} · {f.size}x · X {f.x}, Y {f.y}</small></span></button>{bad && <span className="error-mark" title="Поле выходит за границы">!</span>}<button className="chevron-button" aria-label={`Открыть свойства: ${LABELS[f.kind]}`} onClick={() => { setSelectedId(f.id); setMobilePanel("properties"); }}><Icon name="chevron" /></button></li>; })}</ol>
            {!layout.fields.length && <div className="empty-list">На этикетке пока нет объектов.<button onClick={() => setAddOpen(true)}>Добавить поле</button></div>}
          </section><footer className="panel-foot"><span>{layout.fields.length} полей</span><span>{status}</span></footer>
        </aside>
        <section className={`preview-panel ${mobilePanel === "preview" ? "mobile-visible" : ""}`} aria-label="Превью этикетки"><div className="preview-toolbar"><div className="segmented"><button aria-pressed={view === "edit"} onClick={() => setView("edit")}>Разметка и сетка</button><button aria-pressed={view === "print"} onClick={() => setView("print")}>Как на печати</button></div><div className="zoom-control"><span>Масштаб</span>{(["auto", 4, 6, 8] as const).map(z => <button key={z} aria-pressed={zoom === z} onClick={() => setZoom(z)}>{z === "auto" ? "Авто" : `${z}x`}</button>)}</div></div>
          <div className={`stage ${view === "print" ? "print" : ""}`} onPointerMove={pointerMove} onPointerUp={pointerUp} onPointerCancel={pointerUp}><div className="paper-wrap"><div className="paper-label">{layout.width_mm} x {layout.height_mm} мм · {layout.name || "без имени"}</div><div className="label-canvas" style={{ width: layout.width_mm * canvasScale, height: layout.height_mm * canvasScale, backgroundSize: `${5 * canvasScale}px ${5 * canvasScale}px` }} onClick={e => { if (e.target === e.currentTarget) setSelectedId(null); }}>{layout.fields.map(f => { const size = fieldSize(f); const isSelected = selectedId === f.id; const isBad = f.x < 0 || f.y < 0 || f.x + size.w > layout.width_mm || f.y + size.h > layout.height_mm; return <button key={f.id} className={`canvas-field ${isSelected ? "selected" : ""} ${isBad ? "bad" : ""}`} style={{ left: f.x * canvasScale, top: f.y * canvasScale, width: size.w * canvasScale, height: size.h * canvasScale }} onPointerDown={e => pointerDown(e, f)} aria-label={`${LABELS[f.kind]}, X ${f.x}, Y ${f.y}`}><span className="field-tag">{LABELS[f.kind]}</span>{f.kind === "barcode" ? <span className="barcode"><i /><small>{SAMPLES.barcode}</small></span> : <span className="field-text" style={{ fontSize: Math.max(7, size.h * canvasScale * .68), fontWeight: f.kind === "sum" ? 800 : 600 }}>{f.kind === "text" ? f.text : SAMPLES[f.kind]}</span>}{isSelected && view === "edit" && <><i className="handle nw"/><i className="handle ne"/><i className="handle sw"/><i className="handle se"/></>}</button>; })}</div></div></div>
          <footer className="stage-footer"><span>{selected ? `Выбрано: ${LABELS[selected.kind]}` : "Выберите объект на макете или в структуре"}</span><span>Перетаскивание: шаг 0,5 мм</span></footer></section>
        <aside className={`right-panel ${mobilePanel === "properties" ? "mobile-visible" : ""}`} aria-label="Свойства выбранного объекта"><div className="inspector-head"><div><span>Свойства</span><h2>{selected ? LABELS[selected.kind] : "Ничего не выбрано"}</h2></div>{selected && <span className="object-number">#{layout.fields.findIndex(f => f.id === selected.id) + 1}</span>}</div>
          {selected ? <div className="inspector-scroll">{selected.kind === "text" && <section className="property-section"><h3>Содержание</h3><label className="field-control"><span>Текст на этикетке <small>{selected.text?.length || 0} / 48</small></span><input maxLength={48} value={selected.text || ""} onChange={e => patchSelected({ text: e.target.value })} /></label></section>}
            <section className="property-section"><h3>Печатный шрифт</h3>{selected.kind !== "barcode" && <div className="choice-block"><span>Гарнитура</span><div className="choice-grid"><button aria-pressed={selected.font === "A"} onClick={() => patchSelected({ font: "A" })}><b>A</b><small>Крупный · 3x4 мм</small></button><button aria-pressed={selected.font === "B"} onClick={() => patchSelected({ font: "B" })}><b>B</b><small>Компакт · 2x3 мм</small></button></div></div>}<div className="choice-block"><span>Масштаб</span><div className="segmented full"><button aria-pressed={selected.size === 1} onClick={() => patchSelected({ size: 1 })}>1x Обычный</button><button aria-pressed={selected.size === 2} onClick={() => patchSelected({ size: 2 })}>2x Двойной</button></div></div>{selected.kind === "sum" && <p className="property-hint">Итоговая сумма печатается весами в увеличенном масштабе.</p>}</section>
            <section className="property-section"><div className="property-title-row"><h3>Положение</h3><button onClick={() => { const s = fieldSize(selected); patchSelected({ x: snap((layout.width_mm - s.w) / 2) }); }}>По центру X</button></div><div className="field-grid"><NumberField label="Слева (X), мм" value={selected.x} onChange={v => patchSelected({ x: v })} /><NumberField label="Сверху (Y), мм" value={selected.y} onChange={v => patchSelected({ y: v })} /></div></section>
            <section className="property-section"><div className="property-title-row"><h3>Рамка поля</h3><button onClick={() => patchSelected({ w: undefined, h: undefined })}>Авто</button></div><div className="field-grid"><NumberField label="Ширина, мм" value={fieldSize(selected).w} onChange={v => patchSelected({ w: v })} /><NumberField label="Высота, мм" value={fieldSize(selected).h} onChange={v => patchSelected({ h: v })} /></div><p className="property-hint">Ручной размер влияет на область обрезки содержимого.</p></section>
            <section className="property-section"><h3>Порядок печати</h3><div className="order-actions"><button disabled={layout.fields[0]?.id === selected.id} onClick={() => reorder(layout.fields.findIndex(f => f.id === selected.id), -1)}>Выше</button><button disabled={layout.fields.at(-1)?.id === selected.id} onClick={() => reorder(layout.fields.findIndex(f => f.id === selected.id), 1)}>Ниже</button></div></section></div> : <div className="inspector-empty"><div className="empty-icon"><Icon name="sliders" size={22} /></div><h3>Выберите объект</h3><p>Нажмите на поле в структуре слева или прямо на макете. Здесь появятся его параметры.</p></div>}
          {selected && <footer className="inspector-actions"><button className="button ghost" onClick={duplicateSelected}><Icon name="copy" /> Копия</button><button className="button danger" onClick={removeSelected}><Icon name="trash" /> Удалить</button></footer>}</aside>
      </main><input ref={fileRef} type="file" accept=".json,application/json" hidden onChange={e => importFile(e.target.files?.[0])} />
    </div>
  );
}
