use worker::{D1Database, Result};
use worker::wasm_bindgen::JsValue;
use std::rc::Rc;

pub enum LogLevel {
    Info,
    Warn,
    Error,
}

impl LogLevel {
    fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
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
        let prefix = level.as_str();
        let console_msg = format!("[{}] {}", prefix, message);
        
        // 1. Log to Cloudflare Worker Console natively
        match level {
            LogLevel::Info => worker::console_log!("{}", console_msg),
            LogLevel::Warn => worker::console_warn!("{}", console_msg),
            LogLevel::Error => worker::console_error!("{}", console_msg),
        }

        // 2. Fire and Forget to D1 ExecutionLogs Table persistently (if DB initialized)
        if let Some(db) = &self.db {
            let db_clone = Rc::clone(db);
            let prefix_owned = prefix.to_string();
            let msg_owned = message.to_string();
            
            worker::wasm_bindgen_futures::spawn_local(async move {
                if let Err(e) = Self::log_to_db(&db_clone, &prefix_owned, &msg_owned).await {
                    worker::console_error!("[ERROR] Failed to write log to D1 ExecutionLogs: {:?}", e);
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

    /// Internal function handling parameter bounds to write safely to SQLite
    async fn log_to_db(db: &D1Database, level: &str, message: &str) -> Result<()> {
        let stmt = db.prepare("INSERT INTO ExecutionLogs (Level, Message) VALUES (?1, ?2)");
        let params = vec![JsValue::from_str(level), JsValue::from_str(message)];
        
        stmt.bind(&params)?.run().await?;
        Ok(())
    }
}
