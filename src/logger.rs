//! Dual logging: writes to both the Cloudflare Worker console and a persistent D1 `logs` table.
use worker::{D1Database, Result};
use worker::wasm_bindgen::JsValue;
use std::rc::Rc;

pub enum LogLevel {
    Info,
    Warn,
    Error,
}

impl LogLevel {
    /// Returns `(console_label, db_level)` for this log level.
    /// DB levels match schema.sql: 0=Trace, 1=Debug, 2=Info, 3=Warning, 4=Error, 5=Critical
    fn metadata(&self) -> (&'static str, u8) {
        match self {
            LogLevel::Info => ("INFO", 2),
            LogLevel::Warn => ("WARN", 3),
            LogLevel::Error => ("ERROR", 4),
        }
    }
}

pub struct Logger {
    db: Option<Rc<D1Database>>,
}

impl Logger {
    /// Create a new logger. Pass `Some(d1)` to enable logging to the database table,
    /// or `None` to only log to the worker console.
    pub fn new(db: Option<D1Database>) -> Self {
        Self { db: db.map(Rc::new) }
    }

    /// Primary log function that handles both console and D1 logging asynchronously without blocking
    pub fn log(&self, level: LogLevel, message: &str) {
        let (prefix, db_level) = level.metadata();
        let console_msg = format!("[{prefix}] {message}");

        // 1. Log to Cloudflare Worker Console natively
        match level {
            LogLevel::Info => worker::console_log!("{}", console_msg),
            LogLevel::Warn => worker::console_warn!("{}", console_msg),
            LogLevel::Error => worker::console_error!("{}", console_msg),
        }

        // 2. Fire and forget to D1 `logs` table (if DB initialized)
        if let Some(db) = &self.db {
            let db_clone = Rc::clone(db);
            let msg_owned = message.to_string();

            worker::wasm_bindgen_futures::spawn_local(async move {
                if let Err(e) = Self::log_to_db(&db_clone, db_level, &msg_owned).await {
                    worker::console_error!("[ERROR] Failed to write log to D1: {:?}", e);
                }
            });
        }
    }

    pub fn info(&self, message: &str) {
        self.log(LogLevel::Info, message);
    }

    pub fn warn(&self, message: &str) {
        self.log(LogLevel::Warn, message);
    }

    pub fn error(&self, message: &str) {
        self.log(LogLevel::Error, message);
    }

    /// Inserts a log row into the `logs` table with an integer level.
    async fn log_to_db(db: &D1Database, level: u8, message: &str) -> Result<()> {
        let stmt = db.prepare("INSERT INTO logs (Level, Message) VALUES (?1, ?2)");
        let params = vec![JsValue::from(level), JsValue::from_str(message)];

        stmt.bind(&params)?.run().await?;
        Ok(())
    }
}
