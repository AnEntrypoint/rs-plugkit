use rs_plugkit::orchestrator::yaml_util;
use serde_yaml::{Mapping, Value};

fn row_with_why(value: &str) -> Value {
    let mut map = Mapping::new();
    map.insert(Value::String("id".into()), Value::String("guard-probe".into()));
    map.insert(Value::String("why".into()), Value::String(value.into()));
    Value::Sequence(vec![Value::Mapping(map)])
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_default();
    let rest: Vec<String> = args.collect();
    match mode.as_str() {
        "serialize" => {
            let value = rest.join(" ");
            let doc = row_with_why(&value);
            match yaml_util::serialize_checked(&doc) {
                Ok(raw) => {
                    let back: Value = serde_yaml::from_str(&raw).expect("emitted text must parse");
                    let got = back.as_sequence().and_then(|s| s.first())
                        .and_then(|m| m.get("why")).and_then(|v| v.as_str())
                        .unwrap_or("");
                    println!("SERIALIZE_OK needs_quoting={}", yaml_util::needs_quoting(&value));
                    println!("emitted:\n{}", raw.trim_end());
                    println!("roundtrip_value_equal={}", got == value);
                    println!("roundtrip_doc_equal={}", back == doc);
                    if got != value || back != doc {
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    println!("SERIALIZE_REFUSED: {e}");
                    std::process::exit(1);
                }
            }
        }
        "diagnose" | "repair" => {
            let Some(path) = rest.first() else {
                eprintln!("usage: prd_yaml_guard {mode} <path>");
                std::process::exit(2);
            };
            let text = match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) => {
                    println!("READ_FAILED {path}: {e}");
                    std::process::exit(2);
                }
            };
            let prefix = text.strip_prefix('\u{feff}').unwrap_or(&text);
            match serde_yaml::from_str::<Value>(prefix) {
                Ok(doc) => {
                    println!("PARSE_OK rows={} bytes={} path={path}",
                        doc.as_sequence().map(|s| s.len()).unwrap_or(0), text.len());
                    return;
                }
                Err(e) => match yaml_util::parse_fault(prefix, &e) {
                    Some(f) => println!("PARSE_FAIL {} path={path}", f.describe()),
                    None => println!("PARSE_FAIL (no position): {e} path={path}"),
                },
            }
            if mode == "diagnose" {
                return;
            }
            match yaml_util::repair_text(prefix, 16) {
                Ok(plan) => {
                    for f in &plan.faults {
                        println!("FAULT {} -> requoted", f.describe());
                    }
                    let guarded = serde_yaml::from_str::<Value>(&plan.repaired)
                        .ok()
                        .and_then(|d| yaml_util::serialize_checked(&d).ok())
                        .unwrap_or_else(|| plan.repaired.clone());
                    let out = format!("{path}.repaired");
                    std::fs::write(&out, &guarded).expect("write repaired copy");
                    let verify: Value = serde_yaml::from_str(&guarded).expect("repaired copy parses");
                    println!("REPAIR_OK changed={} rows_after={} bytes_before={} bytes_after={} wrote={}",
                        plan.changed,
                        verify.as_sequence().map(|s| s.len()).unwrap_or(0),
                        text.len(),
                        guarded.len(),
                        out);
                }
                Err((why, faults)) => {
                    for f in &faults {
                        println!("FAULT {}", f.describe());
                    }
                    println!("REPAIR_REFUSED: {why}");
                    std::process::exit(1);
                }
            }
        }
        _ => {
            eprintln!("usage: prd_yaml_guard serialize <value> | diagnose <path> | repair <path>");
            std::process::exit(2);
        }
    }
}
