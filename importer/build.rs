use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn parse_schema_file(path: &Path) -> std::collections::HashMap<String, Vec<String>> {
    let sql = fs::read_to_string(path).unwrap_or_else(|e| panic!("Failed to read {}: {}", path.display(), e));
    let mut schemas = std::collections::HashMap::new();
    let mut in_table = None;
    let mut current_cols = Vec::new();

    for line in sql.lines() {
        let line = line.trim();
        let upper_line = line.to_uppercase();
        if line.is_empty() || line.starts_with("--") {
            continue;
        }

        if upper_line.starts_with("CREATE TABLE") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            let mut table_name = "";
            for (i, p) in parts.iter().enumerate() {
                if p.starts_with('(') {
                    table_name = parts.get(i.saturating_sub(1)).unwrap_or(&"");
                    break;
                }
                let p_upper = p.to_uppercase();
                if p_upper == "EXISTS" || (p_upper == "TABLE" && parts.get(i + 1).map(|s| s.to_uppercase()) != Some("IF".to_string())) {
                    table_name = parts.get(i + 1).unwrap_or(&"");
                }
            }
            let table_name = table_name.trim_end_matches('(').to_string();
            if !table_name.is_empty() {
                in_table = Some(table_name.clone());
                current_cols.clear();

                if line.ends_with(");") {
                    if let (Some(start), Some(end)) = (line.find('('), line.rfind(')')) {
                        if start < end {
                            let cols_str = &line[start + 1..end];
                            for col_def in cols_str.split(',') {
                                let col_def = col_def.trim();
                                let col_upper = col_def.to_uppercase();
                                if !col_upper.starts_with("PRIMARY KEY")
                                    && !col_upper.starts_with("FOREIGN KEY")
                                    && !col_upper.starts_with("UNIQUE")
                                    && !col_upper.starts_with("CHECK")
                                    && let Some(col_name) = col_def.split_whitespace().next()
                                {
                                    if !col_name.is_empty() {
                                        current_cols.push(col_name.to_string());
                                    }
                                }
                            }
                        }
                    }
                    schemas.insert(table_name, current_cols.clone());
                    in_table = None;
                }
            }
        } else if in_table.is_some() {
            if line.starts_with(");") || line == ")" {
                if let Some(table) = in_table.take() {
                    schemas.insert(table, current_cols.clone());
                }
            } else {
                if !upper_line.starts_with("PRIMARY KEY")
                    && !upper_line.starts_with("FOREIGN KEY")
                    && !upper_line.starts_with("UNIQUE")
                    && !upper_line.starts_with("CHECK")
                    && let Some(col_name) = line.split_whitespace().next()
                {
                    let col_name = col_name.trim_end_matches(',');
                    if !col_name.is_empty() {
                        current_cols.push(col_name.to_string());
                    }
                }
            }
        }
    }
    schemas
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=../migrations");
    let out_dir = env::var("OUT_DIR")?;
    let dest_path = PathBuf::from(out_dir).join("schemas.rs");

    let mut generated_code = String::new();
    generated_code.push_str("match provider_name {\n");

    let migrations_dir = PathBuf::from("../migrations");
    if migrations_dir.exists() {
        for entry in fs::read_dir(&migrations_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir()
                && let Some(file_name) = path.file_name()
                && let Some(provider_name) = file_name.to_str()
            {
                let schema_path = path.join("0_gtfs_schema.sql");
                if schema_path.exists() {
                    let parsed = parse_schema_file(&schema_path);
                    generated_code.push_str(&format!("    \"{}\" => Some(&[\n", provider_name));
                    for (table, cols) in parsed {
                        generated_code.push_str(&format!("        (\"{}\", &[\n", table));
                        for col in cols {
                            generated_code.push_str(&format!("            \"{}\",\n", col));
                        }
                        generated_code.push_str("        ]),\n");
                    }
                    generated_code.push_str("    ]),\n");
                }
            }
        }
    }

    generated_code.push_str("    _ => None,\n");
    generated_code.push_str("}\n");

    fs::write(&dest_path, generated_code)?;
    Ok(())
}
