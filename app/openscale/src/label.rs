use serde_json::Value;

pub const MM_TO_DOTS: i32 = 8;
const REQUIRED: [LabelKind; 4] = [LabelKind::Name, LabelKind::Weight, LabelKind::Sum, LabelKind::Barcode];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LabelKind {
    Name,
    Plu,
    Barcode,
    Weight,
    Price,
    Sum,
    Date,
    Time,
    Shelflife,
    Cardprice,
    Text,
}

impl LabelKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "name" => Self::Name,
            "plu" => Self::Plu,
            "barcode" => Self::Barcode,
            "weight" => Self::Weight,
            "price" => Self::Price,
            "sum" => Self::Sum,
            "date" => Self::Date,
            "time" => Self::Time,
            "shelflife" => Self::Shelflife,
            "cardprice" => Self::Cardprice,
            "text" => Self::Text,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Plu => "plu",
            Self::Barcode => "barcode",
            Self::Weight => "weight",
            Self::Price => "price",
            Self::Sum => "sum",
            Self::Date => "date",
            Self::Time => "time",
            Self::Shelflife => "shelflife",
            Self::Cardprice => "cardprice",
            Self::Text => "text",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LabelField {
    pub kind: LabelKind,
    pub x_mm: i32,
    pub y_mm: i32,
    pub font_a: bool,
    pub size2: bool,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LabelSpec {
    pub name: String,
    pub width_mm: i32,
    pub height_mm: i32,
    pub fields: Vec<LabelField>,
}

fn i32_field(v: &Value, key: &str) -> Result<i32, String> {
    v.get(key)
        .and_then(Value::as_i64)
        .map(|n| n as i32)
        .ok_or_else(|| format!("поле {key}: ожидается целое число"))
}

fn parse_field(v: &Value, index: usize) -> Result<LabelField, String> {
    let kind_str = v
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("поле {index}: нет kind"))?;
    let kind = LabelKind::parse(kind_str)
        .ok_or_else(|| format!("поле {index}: неизвестный kind \"{kind_str}\""))?;
    let x_mm = i32_field(v, "x")?;
    let y_mm = i32_field(v, "y")?;
    let font = v.get("font").and_then(Value::as_str).unwrap_or("A");
    if font != "A" && font != "B" {
        return Err(format!("поле {index}: font должен быть \"A\" или \"B\""));
    }
    let size = v.get("size").and_then(Value::as_i64).unwrap_or(1);
    if size != 1 && size != 2 {
        return Err(format!("поле {index}: size должен быть 1 или 2"));
    }
    let text = v
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if kind == LabelKind::Text && (text.is_empty() || text.chars().count() > 48) {
        return Err(format!(
            "поле {index}: текст произвольного поля — непустая строка до 48 символов"
        ));
    }
    Ok(LabelField {
        kind,
        x_mm,
        y_mm,
        font_a: font == "A",
        size2: size == 2,
        text,
    })
}

pub fn parse(spec_json: &str) -> Result<LabelSpec, String> {
    let v: Value =
        serde_json::from_str(spec_json).map_err(|e| format!("JSON не разобран: {e}"))?;
    if v.get("format").and_then(Value::as_i64) != Some(1) {
        return Err("format должен быть 1".into());
    }
    let name = v
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if name.is_empty() || name.chars().count() > 64 {
        return Err("name — непустая строка до 64 символов".into());
    }
    let width_mm = i32_field(&v, "width_mm")?;
    let height_mm = i32_field(&v, "height_mm")?;
    if !(20..=80).contains(&width_mm) || !(20..=80).contains(&height_mm) {
        return Err("размер этикетки — от 20 до 80 мм".into());
    }
    let raw_fields = v
        .get("fields")
        .and_then(Value::as_array)
        .ok_or_else(|| "fields должен быть массивом".to_string())?;
    let mut fields = Vec::new();
    for (i, f) in raw_fields.iter().enumerate() {
        let field = parse_field(f, i)?;
        if field.x_mm < 0 || field.y_mm < 0 || field.x_mm >= width_mm || field.y_mm >= height_mm {
            return Err(format!(
                "поле {i} ({}): координаты вне границ этикетки",
                field.kind.name()
            ));
        }
        fields.push(field);
    }
    for req in REQUIRED {
        if !fields.iter().any(|f| f.kind == req) {
            return Err(format!(
                "нет обязательного поля kind \"{}\"",
                req.name()
            ));
        }
    }
    Ok(LabelSpec {
        name,
        width_mm,
        height_mm,
        fields,
    })
}

pub fn choose(specs: &[LabelSpec], len_mm: i32) -> Option<LabelSpec> {
    let mut best: Option<(i32, usize, &LabelSpec)> = None;
    for spec in specs {
        let diff = (spec.height_mm - len_mm).abs();
        if diff > 5 {
            continue;
        }
        let better = best.map_or(true, |(bd, bf, _)| diff < bd || (diff == bd && spec.fields.len() > bf));
        if better {
            best = Some((diff, spec.fields.len(), spec));
        }
    }
    best.map(|(_, _, s)| s.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{
        "format": 1,
        "name": "58x40",
        "width_mm": 58,
        "height_mm": 40,
        "fields": [
            { "kind": "name",  "x": 2,  "y": 1,  "font": "B", "size": 1 },
            { "kind": "weight", "x": 2, "y": 21, "font": "A", "size": 1 },
            { "kind": "price", "x": 2,  "y": 26, "font": "A", "size": 1 },
            { "kind": "sum",   "x": 2,  "y": 31, "font": "A", "size": 2 },
            { "kind": "barcode", "x": 2, "y": 5, "font": "A", "size": 1 },
            { "kind": "text", "x": 10, "y": 36, "font": "B", "size": 1, "text": "СПАСИБО ЗА ПОКУПКУ" }
        ]
    }"#;

    fn minimal() -> String {
        r#"{"format":1,"name":"58x40","width_mm":58,"height_mm":40,"fields":[
            {"kind":"name","x":2,"y":1,"font":"A","size":1},
            {"kind":"weight","x":2,"y":21,"font":"A","size":1},
            {"kind":"sum","x":2,"y":31,"font":"A","size":1},
            {"kind":"barcode","x":2,"y":5,"font":"A","size":1}]}"#
            .into()
    }

    #[test]
    fn parse_good_spec_with_all_kinds() {
        let spec = parse(GOOD).unwrap();
        assert_eq!(spec.name, "58x40");
        assert_eq!((spec.width_mm, spec.height_mm), (58, 40));
        assert_eq!(spec.fields.len(), 6);
        let text = spec.fields.iter().find(|f| f.kind == LabelKind::Text).unwrap();
        assert_eq!(text.text, "СПАСИБО ЗА ПОКУПКУ");
        assert!(!text.font_a);
        let sum = spec.fields.iter().find(|f| f.kind == LabelKind::Sum).unwrap();
        assert!(sum.size2);
    }

    #[test]
    fn parse_rejects_wrong_format_version() {
        let bad = minimal().replace("\"format\":1", "\"format\":2");
        assert_eq!(parse(&bad).unwrap_err(), "format должен быть 1");
    }

    #[test]
    fn parse_rejects_missing_required_kind() {
        let bad = minimal().replace(r#"{"kind":"sum","x":2,"y":31,"font":"A","size":1},"#, "");
        let err = parse(&bad).unwrap_err();
        assert!(err.contains("sum"), "{err}");
    }

    #[test]
    fn parse_rejects_unknown_kind() {
        let bad = minimal().replace("\"kind\":\"sum\"", "\"kind\":\"vat\"");
        let err = parse(&bad).unwrap_err();
        assert!(err.contains("неизвестный kind"), "{err}");
    }

    #[test]
    fn parse_rejects_bad_text() {
        let empty = GOOD.replace("СПАСИБО ЗА ПОКУПКУ", "");
        assert!(parse(&empty).unwrap_err().contains("текст"));
        let long = format!(
            r#"{{"format":1,"name":"58x40","width_mm":58,"height_mm":40,"fields":[
            {{"kind":"name","x":2,"y":1,"font":"A","size":1}},
            {{"kind":"weight","x":2,"y":21,"font":"A","size":1}},
            {{"kind":"sum","x":2,"y":31,"font":"A","size":1}},
            {{"kind":"barcode","x":2,"y":5,"font":"A","size":1}},
            {{"kind":"text","x":10,"y":36,"font":"B","size":1,"text":"{}"}}]}}"#,
            "Ж".repeat(49)
        );
        assert!(parse(&long).unwrap_err().contains("48"));
    }

    #[test]
    fn parse_rejects_out_of_bounds() {
        let bad = minimal().replace("\"y\":31", "\"y\":40");
        let err = parse(&bad).unwrap_err();
        assert!(err.contains("вне границ"), "{err}");
        let neg = minimal().replace("\"x\":2,\"y\":31", "\"x\":-1,\"y\":31");
        assert!(parse(&neg).unwrap_err().contains("вне границ"));
    }

    #[test]
    fn parse_rejects_bad_size_dimensions_and_name() {
        let size3 = minimal().replace("\"size\":1},", "\"size\":3},");
        assert!(parse(&size3).unwrap_err().contains("size"));
        let font = minimal().replace("\"font\":\"A\"", "\"font\":\"C\"");
        assert!(parse(&font).unwrap_err().contains("font"));
        let small = minimal().replace("\"width_mm\":58", "\"width_mm\":10");
        assert!(parse(&small).unwrap_err().contains("от 20 до 80"));
        let noname = minimal().replace("\"name\":\"58x40\"", "\"name\":\" \"");
        assert!(parse(&noname).unwrap_err().contains("name"));
    }

    #[test]
    fn parse_rejects_broken_json() {
        assert!(parse("{").unwrap_err().contains("не разобран"));
        let nofields = r#"{"format":1,"name":"x","width_mm":58,"height_mm":40}"#;
        assert!(parse(nofields).unwrap_err().contains("массивом"));
    }

    #[test]
    fn choose_picks_within_5mm_nearest_then_richest() {
        let a = parse(&minimal().replace("58x40", "40a")).unwrap();
        let b_spec = minimal().replace("\"height_mm\":40", "\"height_mm\":38").replace("58x40", "38b").replace(
            r#"{"kind":"barcode","x":2,"y":5,"font":"A","size":1}]"#,
            r#"{"kind":"barcode","x":2,"y":5,"font":"A","size":1},{"kind":"price","x":2,"y":26,"font":"A","size":1}]"#,
        );
        let b = parse(&b_spec).unwrap();
        let far = parse(&minimal().replace("\"height_mm\":40", "\"height_mm\":50").replace("58x40", "50c")).unwrap();
        let specs = vec![a.clone(), b.clone(), far];
        assert_eq!(choose(&specs, 40).unwrap().name, "40a");
        assert_eq!(choose(&specs, 38).unwrap().name, "38b");
        assert_eq!(choose(&specs, 39).unwrap().name, "38b");
        assert!(choose(&specs, 60).is_none());
    }

    #[test]
    fn choose_tie_prefers_more_fields() {
        let sparse = minimal().replace("58x40", "s");
        let rich = GOOD.replace("58x40", "r");
        let s = parse(&sparse).unwrap();
        let r = parse(&rich).unwrap();
        let picked = choose(&[s, r], 40).unwrap();
        assert_eq!(picked.fields.len(), 6);
    }
}
