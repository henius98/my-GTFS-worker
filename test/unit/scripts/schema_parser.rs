use super::*;

#[test]
fn test_split_top_level_comma_nested_parentheses_and_composite_keys() {
  let input = "Id INTEGER PRIMARY KEY, Level TINYINT NOT NULL CHECK (Level IN (0, 1, 2, 3, 4, 5)), Message TEXT NOT NULL, PRIMARY KEY (Id, Level)";
  let parts = split_top_level_comma(input);
  assert_eq!(parts.len(), 4);
  assert_eq!(parts[0].trim(), "Id INTEGER PRIMARY KEY");
  assert_eq!(parts[1].trim(), "Level TINYINT NOT NULL CHECK (Level IN (0, 1, 2, 3, 4, 5))");
  assert_eq!(parts[2].trim(), "Message TEXT NOT NULL");
  assert_eq!(parts[3].trim(), "PRIMARY KEY (Id, Level)");
}

#[test]
fn test_strip_sql_comments_block_comment_spacing() {
  let sql = "CREATE/*comment*/TABLE foo (id INT);";
  assert_eq!(strip_sql_comments(sql), "CREATE TABLE foo (id INT);");
}

#[test]
fn test_split_sql_statements_ignores_semicolons_in_comments_and_strings() {
  let sql = r#"
    -- Calculated departures; empty route_id and direction_id -1 mean no filter.
    CREATE TABLE departure_cache (
      note TEXT DEFAULT 'a;b'
    );
    /* multi-line; comment; with semicolons */
    CREATE INDEX idx_test ON departure_cache (note);
  "#;
  let statements = split_sql_statements(sql);
  assert_eq!(statements.len(), 2);
  assert!(statements[0].starts_with("CREATE TABLE departure_cache"));
  assert!(statements[0].contains("'a;b'"));
  assert_eq!(statements[1], "CREATE INDEX idx_test ON departure_cache (note)");
}

fn assert_tables(sql: &str, expected: &[(&str, &[&str], &[&str])]) {
  let schemas = parse_schema_str(sql);
  let pks = parse_primary_keys_str(sql);
  let to_owned_vec = |items: &[&str]| items.iter().map(|item| (*item).to_owned()).collect::<Vec<_>>();
  for (table, cols, pk_cols) in expected {
    assert_eq!(schemas.get(*table), Some(&to_owned_vec(cols)), "columns for {table}");
    assert_eq!(pks.get(*table), Some(&to_owned_vec(pk_cols)), "primary keys for {table}");
  }
}

#[test]
fn test_parse_schema_and_primary_keys_composite_and_inline() {
  let sql = r#"
    -- PRIMARY KEY in comment
    CREATE TABLE IF NOT EXISTS test_table (
      id TEXT,
      trip_id TEXT,
      note TEXT DEFAULT 'PRIMARY KEY', -- comment: not a PRIMARY KEY
      status INTEGER,
      FOREIGN KEY (trip_id)
        REFERENCES trips (trip_id),
      PRIMARY KEY (
        id,
        trip_id
      )
    );
    CREATE TABLE IF NOT EXISTS single_pk (
      code TEXT PRIMARY KEY,
      description TEXT
    );
    CREATE TABLE IF NOT EXISTS no_pk (
      val1 TEXT,
      val2 TEXT
    );
  "#;
  assert_tables(sql, &[("test_table", &["id", "trip_id", "note", "status"], &["id", "trip_id"]), ("single_pk", &["code", "description"], &["code"]), ("no_pk", &["val1", "val2"], &[])]);
}

#[test]
fn test_parse_table_multiline_whitespace_and_quoted_names() {
  let sql = r#"
    CREATE
    TABLE  "bus routes" (
      "route id" TEXT PRIMARY KEY,
      route_name TEXT
    );
    create   table   [train schedules] (
      [schedule_id] TEXT PRIMARY KEY,
      dept_time TEXT
    );
  "#;
  assert_tables(sql, &[("bus routes", &["route id", "route_name"], &["route id"]), ("train schedules", &["schedule_id", "dept_time"], &["schedule_id"])]);
}

#[test]
fn test_parse_schema_check_and_constraint_prefixed_columns() {
  let sql = r#"
    CREATE TABLE IF NOT EXISTS test_check (
      check_id INTEGER PRIMARY KEY,
      checkpoint TEXT,
      unique_code TEXT,
      constraint_name TEXT,
      [bracket_col] INTEGER,
      status TINYINT CHECK (status IN (0, 1)),
      CHECK (check_id > 0)
    );
  "#;
  assert_tables(sql, &[("test_check", &["check_id", "checkpoint", "unique_code", "constraint_name", "bracket_col", "status"], &["check_id"])]);
}

#[test]
fn test_parse_composite_primary_key_modifiers_and_utf8() {
  let sql = r#"
    CREATE TABLE IF NOT EXISTS trips_utf8 (
      "路线_id" TEXT,
      service_id TEXT,
      start_date INTEGER,
      end_date INTEGER,
      CONSTRAINT "pk_路线" PRIMARY KEY (service_id ASC, [start_date] DESC, "路线_id" COLLATE NOCASE)
    );
  "#;
  assert_tables(sql, &[("trips_utf8", &["路线_id", "service_id", "start_date", "end_date"], &["service_id", "start_date", "路线_id"])]);
}

#[test]
fn test_parse_schema_and_primary_keys_multiline_and_spacing() {
  let sql = r#"
    CREATE TABLE IF NOT EXISTS multiline_pk (
      service_id TEXT,
      date INTEGER,
      PRIMARY
        KEY (
          service_id,
          date
        )
    );
    CREATE TABLE IF NOT EXISTS inline_multiline_pk (
      trip_id TEXT PRIMARY
        KEY,
      direction_id INTEGER,
      FOREIGN
        KEY (direction_id) REFERENCES directions (direction_id)
    );
    CREATE TABLE IF NOT EXISTS no_space_table(
      id INTEGER PRIMARY KEY
    );
    CREATE TABLE IF NOT EXISTS "quoted (paren) table" (
      col1 TEXT PRIMARY KEY
    );
  "#;
  assert_tables(
    sql,
    &[
      ("multiline_pk", &["service_id", "date"], &["service_id", "date"]),
      ("inline_multiline_pk", &["trip_id", "direction_id"], &["trip_id"]),
      ("no_space_table", &["id"], &["id"]),
      ("quoted (paren) table", &["col1"], &["col1"]),
    ],
  );
}

#[test]
fn test_parse_staging_create_statements_rewrites_table_name_and_preserves_options() {
  let sql = r#"
    -- comment before table
    CREATE TABLE IF NOT EXISTS stop_times (
      stop_id TEXT,
      trip_id TEXT,
      stop_sequence INTEGER,
      PRIMARY KEY (stop_id, trip_id, stop_sequence)
    ) WITHOUT ROWID;
    CREATE TABLE IF NOT EXISTS no_space(id INTEGER PRIMARY KEY) STRICT;
    INSERT OR IGNORE INTO foo VALUES (1);
  "#;
  let staging = parse_staging_create_statements(sql, STAGING_TABLE_SUFFIX);
  assert_eq!(staging.len(), 2);
  let stop_times_sql = staging.get("stop_times").map(String::as_str).unwrap_or_default();
  assert!(stop_times_sql.starts_with("CREATE TABLE \"stop_times__staging\" ("));
  assert!(stop_times_sql.ends_with(") WITHOUT ROWID"));
  assert!(!stop_times_sql.contains("IF NOT EXISTS"));
  assert_eq!(staging.get("no_space").map(String::as_str), Some("CREATE TABLE \"no_space__staging\"(id INTEGER PRIMARY KEY) STRICT"));
}

#[test]
fn test_parse_staging_writes_per_row_distinguishes_without_rowid_and_indexed_tables() {
  let sql = r#"
    CREATE TABLE IF NOT EXISTS stops (
      stop_id TEXT PRIMARY KEY,
      unique_code TEXT,
      note TEXT DEFAULT 'UNIQUE'
    ) WITHOUT ROWID;
    CREATE TABLE IF NOT EXISTS strict_table (
      id TEXT PRIMARY KEY,
      val TEXT
    ) WITHOUT ROWID, STRICT;
    CREATE TABLE IF NOT EXISTS with_unique_col (
      id TEXT PRIMARY KEY,
      code TEXT UNIQUE
    ) WITHOUT ROWID;
    CREATE TABLE IF NOT EXISTS with_unique_constraint (
      id TEXT PRIMARY KEY,
      code TEXT,
      CONSTRAINT uq_code UNIQUE (code)
    ) WITHOUT ROWID;
    CREATE TABLE IF NOT EXISTS rowid_table (
      id TEXT PRIMARY KEY,
      val TEXT
    );
  "#;
  let writes = parse_staging_writes_per_row(sql, STAGING_TABLE_SUFFIX);
  assert_eq!(writes.get("stops"), Some(&1));
  assert_eq!(writes.get("strict_table"), Some(&1));
  assert_eq!(writes.get("with_unique_col"), Some(&2));
  assert_eq!(writes.get("with_unique_constraint"), Some(&2));
  assert_eq!(writes.get("rowid_table"), Some(&2));
}
