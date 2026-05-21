use rusqlite::{params, Connection, Result};
use std::sync::{Arc, Mutex};
use crate::crypto;

pub struct Db {
    pub conn: Arc<Mutex<Connection>>,
}

impl Db {
    pub fn new() -> Result<Self> {
        let conn = Connection::open("os-watchdog.db")?;
        
        conn.execute(
            "CREATE TABLE IF NOT EXISTS users (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                username TEXT UNIQUE NOT NULL,
                password_hash TEXT NOT NULL,
                must_change_password BOOLEAN NOT NULL DEFAULT 0
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ip TEXT NOT NULL,
                port INTEGER NOT NULL,
                ssh_port INTEGER NOT NULL DEFAULT 22,
                hostname TEXT,
                group_name TEXT,
                auth_type TEXT NOT NULL,
                auth_data BLOB NOT NULL
            )",
            [],
        )?;

        // DB Migration for existing databases
        let _ = conn.execute("ALTER TABLE nodes ADD COLUMN ssh_port INTEGER NOT NULL DEFAULT 22", []);

        conn.execute(
            "CREATE TABLE IF NOT EXISTS metrics (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                node_id INTEGER NOT NULL,
                timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
                cpu_usage REAL,
                mem_total INTEGER,
                mem_used INTEGER,
                power_w REAL,
                FOREIGN KEY(node_id) REFERENCES nodes(id)
            )",
            [],
        )?;

        // Ensure default admin user exists
        let mut count: i32 = 0;
        conn.query_row("SELECT COUNT(*) FROM users", [], |row| {
            count = row.get(0)?;
            Ok(())
        })?;

        if count == 0 {
            let hash = crypto::hash_password("admin");
            conn.execute(
                "INSERT INTO users (username, password_hash, must_change_password) VALUES (?1, ?2, ?3)",
                params!["admin", hash, true],
            )?;
            println!("Default admin user created. Please login and change password.");
        }

        Ok(Db {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn prune_metrics(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM metrics WHERE timestamp <= datetime('now', '-6 hours')",
            [],
        )?;
        Ok(())
    }
}
