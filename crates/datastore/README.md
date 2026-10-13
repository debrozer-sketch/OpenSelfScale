# datastore

SQLite-база PLU и идемпотентный импорт из `plu.xml` старого формата.

Формат унаследован: цены в копейках, `plunumber/price/itemcode/namefirst/
namesecond/groupcode/shelflife`. Никакого шифрования и регенераций базы —
замена старого зашифрованного SQLiteCIPHER и процедуры `reg_item.ps1`
(см. `audit/PROBLEMS.md` Б1).

## Использование

```rust
let conn = datastore::open(Path::new("/home/scales/plu.db"))?;
let items = datastore::parse_plu_xml(&xml)?;
datastore::import_plu(&conn, &items)?;
```

Импорт — один REPLACE-батч в транзакции; частично применённый набор невозможен.
