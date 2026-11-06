fn main() -> Result<(), Box<dyn std::error::Error>> {
    let src = std::env::args().nth(1).expect("путь к plu.xml");
    let dst = std::env::args().nth(2).unwrap_or_else(|| "plu.db".into());
    let xml = std::fs::read_to_string(&src)?;
    let items = datastore::parse_plu_xml(&xml)?;
    println!("разобрано товаров: {}", items.len());
    let conn = datastore::open(std::path::Path::new(&dst))?;
    let n = datastore::import_plu(&conn, &items)?;
    println!("импортировано: {n}");
    Ok(())
}
