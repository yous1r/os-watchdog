mod data;
mod deploy;
mod crypto;
mod db;
mod config;

use config::Config;

use axum::{
    routing::{get, post}, 
    Router, 
    response::{IntoResponse, Html, Response}, 
    Json,
    extract::{State, Request},
    middleware::{self, Next},
    http::{StatusCode, header},
};
use jsonwebtoken::{encode, decode, Header, Validation, EncodingKey, DecodingKey};
use serde::{Deserialize, Serialize};
use db::Db;
use clap::Parser;
use tower_http::cors::{CorsLayer, Any};
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Gauge, Row, Table},
    Terminal,
};
use std::{
    collections::HashMap, 
    error::Error, 
    fs, 
    io, 
    sync::{Arc, RwLock},
    time::{Duration, Instant},
    process::Command,
};
use sysinfo::{System, Networks, Disks, Components};
use get_if_addrs::get_if_addrs;
use data::{SystemData, NetworkInfo, DiskInfo, DiskType, SmartData, ComponentInfo};

fn get_disk_type(name: &str) -> DiskType {
    if name.starts_with("nvme") {
        return DiskType::NVMe;
    }
    
    if let Ok(rotational) = fs::read_to_string(format!("/sys/block/{}/queue/rotational", name)) {
        if rotational.trim() == "1" {
            return DiskType::Hdd;
        } else if rotational.trim() == "0" {
            return DiskType::SataSsd;
        }
    }
    DiskType::Unknown
}

fn get_smart_data(name: &str) -> Option<SmartData> {
    let output = Command::new("sudo")
        .args(&["-n", "smartctl", "-j", "-a", &format!("/dev/{}", name)])
        .output()
        .ok()?;
        
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    
    let passed = json.get("smart_status")
        .and_then(|s| s.get("passed"))
        .and_then(|p| p.as_bool())
        .unwrap_or(false);
        
    let temperature = json.get("temperature")
        .and_then(|t| t.get("current"))
        .and_then(|c| c.as_i64());
        
    let power_on_hours = json.get("power_on_time")
        .and_then(|p| p.get("hours"))
        .and_then(|h| h.as_u64());
        
    Some(SmartData { passed, temperature, power_on_hours })
}

#[derive(Default)]
struct DiskStats {
    read_bytes: u64,
    write_bytes: u64,
}

fn read_diskstats() -> HashMap<String, DiskStats> {
    let mut stats = HashMap::new();
    if let Ok(content) = fs::read_to_string("/proc/diskstats") {
        for line in content.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 14 {
                let name = parts[2].to_string();
                if name.starts_with("loop") || name.starts_with("ram") {
                    continue;
                }
                if let (Ok(r_sectors), Ok(w_sectors)) = (parts[5].parse::<u64>(), parts[9].parse::<u64>()) {
                    stats.insert(name, DiskStats {
                        read_bytes: r_sectors * 512,
                        write_bytes: w_sectors * 512,
                    });
                }
            }
        }
    }
    stats
}

type SharedState = Arc<RwLock<SystemData>>;

fn collector_loop(state: SharedState) {
    let mut sys = System::new_all();
    let mut networks = Networks::new_with_refreshed_list();
    let mut disks = Disks::new_with_refreshed_list();
    let mut components = Components::new_with_refreshed_list();
    
    let mut prev_diskstats = read_diskstats();
    let mut smart_cache: HashMap<String, (Instant, Option<SmartData>)> = HashMap::new();
    let smart_cache_duration = Duration::from_secs(60);

    let read_cpu_energy_uj = || -> Option<u64> {
        fs::read_to_string("/sys/class/powercap/intel-rapl/intel-rapl:0/energy_uj")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
    };
    
    let mut prev_energy = read_cpu_energy_uj();
    let mut prev_time = Instant::now();
    
    loop {
        sys.refresh_all();
        networks.refresh_list();
        networks.refresh();
        disks.refresh_list();
        disks.refresh();
        components.refresh_list();
        components.refresh();
        
        let current_diskstats = read_diskstats();
        
        let mut ip_map = HashMap::new();
        if let Ok(interfaces) = get_if_addrs() {
            for iface in interfaces {
                ip_map.insert(iface.name.clone(), iface.addr.ip().to_string());
            }
        }

        let mut net_info = Vec::new();
        for (interface_name, data) in networks.iter() {
            net_info.push(NetworkInfo {
                interface: interface_name.clone(),
                ip: ip_map.get(interface_name).cloned().unwrap_or_else(|| "Unknown".to_string()),
                mac: format!("{:?}", data.mac_address()),
                rx_bytes_per_sec: data.received(),
                tx_bytes_per_sec: data.transmitted(),
            });
        }
        
        let mut final_disks = Vec::new();
        for (name, current) in &current_diskstats {
            let mut mount_point = String::new();
            let mut total_space = 0;
            let mut available_space = 0;
            
            for disk in disks.iter() {
                let d_name = disk.name().to_string_lossy();
                if d_name.contains(name) {
                    mount_point = disk.mount_point().to_string_lossy().to_string();
                    total_space = disk.total_space();
                    available_space = disk.available_space();
                    break;
                }
            }
            
            let prev = prev_diskstats.get(name).unwrap_or(current);
            let read_diff = current.read_bytes.saturating_sub(prev.read_bytes);
            let write_diff = current.write_bytes.saturating_sub(prev.write_bytes);
            
            let disk_type = get_disk_type(name);
            let smart_data = {
                let cache_entry = smart_cache.entry(name.clone()).or_insert_with(|| {
                    (Instant::now().checked_sub(smart_cache_duration).unwrap_or_else(|| Instant::now()), None)
                });
                
                if cache_entry.0.elapsed() >= smart_cache_duration {
                    cache_entry.1 = get_smart_data(name);
                    cache_entry.0 = Instant::now();
                }
                cache_entry.1.clone()
            };
            
            final_disks.push(DiskInfo {
                name: name.clone(),
                mount_point,
                disk_type,
                smart_data,
                total_space,
                available_space,
                read_bytes_per_sec: read_diff,
                write_bytes_per_sec: write_diff,
            });
        }
        
        for disk in disks.iter() {
            let d_name = disk.name().to_string_lossy().to_string();
            if !final_disks.iter().any(|d| d_name.contains(&d.name)) {
                let disk_type = get_disk_type(&d_name);
                let smart_data = {
                    let cache_entry = smart_cache.entry(d_name.clone()).or_insert_with(|| {
                        (Instant::now().checked_sub(smart_cache_duration).unwrap_or_else(|| Instant::now()), None)
                    });
                    
                    if cache_entry.0.elapsed() >= smart_cache_duration {
                        cache_entry.1 = get_smart_data(&d_name);
                        cache_entry.0 = Instant::now();
                    }
                    cache_entry.1.clone()
                };

                final_disks.push(DiskInfo {
                    name: d_name,
                    mount_point: disk.mount_point().to_string_lossy().to_string(),
                    disk_type,
                    smart_data,
                    total_space: disk.total_space(),
                    available_space: disk.available_space(),
                    read_bytes_per_sec: 0,
                    write_bytes_per_sec: 0,
                });
            }
        }

        let current_energy = read_cpu_energy_uj();
        let current_time = Instant::now();
        let power_w = if let (Some(curr), Some(prev)) = (current_energy, prev_energy) {
            let diff_uj = curr.saturating_sub(prev);
            let duration_s = current_time.duration_since(prev_time).as_secs_f64();
            if duration_s > 0.0 {
                Some((diff_uj as f64) / 1_000_000.0 / duration_s)
            } else {
                None
            }
        } else {
            None
        };
        prev_energy = current_energy;
        prev_time = current_time;

        let mut comp_info = Vec::new();
        for comp in &components {
            comp_info.push(ComponentInfo {
                label: comp.label().to_string(),
                temperature: comp.temperature(),
            });
        }

        {
            let mut data = state.write().unwrap();
            data.cpu_usage = sys.global_cpu_info().cpu_usage();
            data.mem_total = sys.total_memory();
            data.mem_used = sys.used_memory();
            data.power_w = power_w;
            data.temperatures = comp_info.clone();
            data.networks = net_info;
            data.disks = final_disks;
        }

        // Insert local metrics into SQLite
        {
            let data = state.read().unwrap();
            if let Some(db_conn) = data.db.as_ref() {
                // Assuming local node is node_id = 1 (we'll ensure it exists or skip if not found, wait. Node 1 isn't guaranteed local. We'll skip local metrics in DB for now to keep it simple, or we can just call `db_conn.prune_metrics()` here).
                let _ = db_conn.prune_metrics();
            }
        }
        
        prev_diskstats = current_diskstats;
        std::thread::sleep(Duration::from_millis(1000));
    }
}

async fn api_metrics(axum::extract::State(state): axum::extract::State<SharedState>) -> impl IntoResponse {
    let data = {
        let lock = state.read().unwrap();
        lock.clone()
    };
    Json(data)
}

async fn serve_html() -> Html<&'static str> {
    Html(include_str!("index.html"))
}

async fn api_deploy(
    State(state): State<SharedState>,
    Json(req): Json<deploy::DeployRequest>
) -> impl IntoResponse {
    // Clone req data we need to insert later before moving req into deploy_agent
    let ip = req.ip.clone();
    let port = req.port;
    let ssh_port = req.ssh_port;
    let group_name = req.group_name.clone();
    let auth_type = if req.private_key.is_some() { "key".to_string() } else { "password".to_string() };
    
    // We also need the raw secret text to encrypt
    let auth_secret = if auth_type == "key" {
        req.private_key.clone().unwrap_or_default()
    } else {
        req.password.clone().unwrap_or_default()
    };

    let res = deploy::deploy_agent(req).await;
    
    if res.success {
        // Insert into database
        let data = state.read().unwrap();
        if let Some(db_arc) = &data.db {
            let conn = db_arc.conn.lock().unwrap();
            let encrypted_data = crypto::encrypt_data(&auth_secret);
            let _ = conn.execute(
                "INSERT INTO nodes (ip, port, ssh_port, hostname, group_name, auth_type, auth_data) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![ip, port, ssh_port, Option::<String>::None, group_name, auth_type, encrypted_data],
            );
        }
    }
    
    Json(res)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Claims {
    sub: String, // username
    exp: usize,
    must_change_password: bool,
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Serialize)]
struct LoginResponse {
    token: String,
    must_change_password: bool,
}

#[derive(Deserialize)]
struct ChangePasswordRequest {
    new_password: String,
}

async fn api_login(
    State(state): State<SharedState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, (StatusCode, String)> {
    let data = state.read().unwrap();
    let db_arc = data.db.as_ref().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB unavailable".to_string()))?;
    let conn = db_arc.conn.lock().unwrap();

    let mut hash = String::new();
    let mut must_change = false;
    let query_res = conn.query_row(
        "SELECT password_hash, must_change_password FROM users WHERE username = ?1",
        rusqlite::params![req.username],
        |row| {
            hash = row.get(0)?;
            must_change = row.get(1)?;
            Ok(())
        },
    );

    if query_res.is_err() || !crypto::verify_password(&hash, &req.password) {
        return Err((StatusCode::UNAUTHORIZED, "Invalid username or password".to_string()));
    }

    let exp = chrono::Utc::now().checked_add_signed(chrono::Duration::hours(24)).unwrap().timestamp() as usize;
    let claims = Claims {
        sub: req.username.clone(),
        exp,
        must_change_password: must_change,
    };

    // For simplicity, we use a fixed secret key since this is a local tool
    let token = encode(&Header::default(), &claims, &EncodingKey::from_secret(b"os-watchdog-secret"))
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Failed to create token".to_string()))?;

    Ok(Json(LoginResponse { token, must_change_password: must_change }))
}

async fn api_change_password(
    State(state): State<SharedState>,
    axum::extract::Extension(claims): axum::extract::Extension<Claims>,
    Json(req): Json<ChangePasswordRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let data = state.read().unwrap();
    let db_arc = data.db.as_ref().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB unavailable".to_string()))?;
    let conn = db_arc.conn.lock().unwrap();

    let new_hash = crypto::hash_password(&req.new_password);
    
    conn.execute(
        "UPDATE users SET password_hash = ?1, must_change_password = 0 WHERE username = ?2",
        rusqlite::params![new_hash, claims.sub],
    ).map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Failed to update password".to_string()))?;

    let exp = chrono::Utc::now().checked_add_signed(chrono::Duration::hours(24)).unwrap().timestamp() as usize;
    let new_claims = Claims {
        sub: claims.sub.clone(),
        exp,
        must_change_password: false,
    };

    let token = encode(&Header::default(), &new_claims, &EncodingKey::from_secret(b"os-watchdog-secret"))
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Failed to create token".to_string()))?;

    Ok(Json(serde_json::json!({ "success": true, "token": token })))
}

#[derive(Serialize)]
struct NodeInfo {
    id: i64,
    ip: String,
    port: u16,
    ssh_port: u16,
    hostname: Option<String>,
    group_name: Option<String>,
    auth_type: String,
}

async fn api_nodes(State(state): State<SharedState>) -> Result<Json<Vec<NodeInfo>>, (StatusCode, String)> {
    let data = state.read().unwrap();
    let db_arc = data.db.as_ref().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB unavailable".to_string()))?;
    let conn = db_arc.conn.lock().unwrap();

    let mut stmt = conn.prepare("SELECT id, ip, port, ssh_port, hostname, group_name, auth_type FROM nodes")
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Query failed".to_string()))?;
    
    let nodes_iter = stmt.query_map([], |row| {
        Ok(NodeInfo {
            id: row.get(0)?,
            ip: row.get(1)?,
            port: row.get(2)?,
            ssh_port: row.get(3)?,
            hostname: row.get(4)?,
            group_name: row.get(5)?,
            auth_type: row.get(6)?,
        })
    }).map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Map failed".to_string()))?;

    let mut nodes = Vec::new();
    for node in nodes_iter {
        if let Ok(n) = node {
            nodes.push(n);
        }
    }
    
    Ok(Json(nodes))
}

#[derive(Deserialize)]
struct UpdateNodeRequest {
    ip: String,
    port: u16,
    ssh_port: u16,
    hostname: Option<String>,
    group_name: Option<String>,
    auth_type: String,
    auth_secret: Option<String>,
}

async fn api_update_node(
    State(state): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Json(req): Json<UpdateNodeRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let data = state.read().unwrap();
    let db_arc = data.db.as_ref().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB unavailable".to_string()))?;
    let conn = db_arc.conn.lock().unwrap();

    if let Some(secret) = &req.auth_secret {
        if !secret.is_empty() {
            let encrypted_data = crypto::encrypt_data(secret);
            conn.execute(
                "UPDATE nodes SET ip = ?1, port = ?2, ssh_port = ?3, hostname = ?4, group_name = ?5, auth_type = ?6, auth_data = ?7 WHERE id = ?8",
                rusqlite::params![req.ip, req.port, req.ssh_port, req.hostname, req.group_name, req.auth_type, encrypted_data, id],
            ).map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Failed to update node".to_string()))?;
            return Ok(Json(serde_json::json!({ "success": true })));
        }
    }
    
    conn.execute(
        "UPDATE nodes SET ip = ?1, port = ?2, ssh_port = ?3, hostname = ?4, group_name = ?5, auth_type = ?6 WHERE id = ?7",
        rusqlite::params![req.ip, req.port, req.ssh_port, req.hostname, req.group_name, req.auth_type, id],
    ).map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Failed to update node".to_string()))?;

    Ok(Json(serde_json::json!({ "success": true })))
}

async fn api_delete_node(
    State(state): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let data = state.read().unwrap();
    let db_arc = data.db.as_ref().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB unavailable".to_string()))?;
    let conn = db_arc.conn.lock().unwrap();

    let _ = conn.execute("DELETE FROM metrics WHERE node_id = ?1", rusqlite::params![id]);
    
    conn.execute("DELETE FROM nodes WHERE id = ?1", rusqlite::params![id])
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete node".to_string()))?;

    Ok(Json(serde_json::json!({ "success": true })))
}

async fn auth_middleware(
    mut req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let path = req.uri().path();
    if path == "/api/login" || path == "/" {
        return Ok(next.run(req).await);
    }

    let auth_header = req.headers().get(header::AUTHORIZATION).and_then(|h| h.to_str().ok());
    if let Some(auth_header) = auth_header {
        if auth_header.starts_with("Bearer ") {
            let token = &auth_header[7..];
            if let Ok(token_data) = decode::<Claims>(token, &DecodingKey::from_secret(b"os-watchdog-secret"), &Validation::default()) {
                if path != "/api/change_password" && token_data.claims.must_change_password {
                    return Err(StatusCode::FORBIDDEN); // Must change password first
                }
                req.extensions_mut().insert(token_data.claims);
                return Ok(next.run(req).await);
            }
        }
    }
    
    Err(StatusCode::UNAUTHORIZED)
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, help = "Path to configuration file")]
    config: Option<String>,
    #[arg(long, help = "Run only as a background agent without TUI")]
    agent_only: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    
    let config = if let Some(config_path) = &args.config {
        Config::load(std::path::Path::new(config_path)).unwrap_or_default()
    } else {
        Config::default()
    };
    
    let db = if !args.agent_only {
        let database = Db::new().expect("Failed to initialize database");
        
        // Merge config nodes into DB
        {
            let conn = database.conn.lock().unwrap();
            for node in &config.nodes {
                // Check if node already exists by IP and Agent port
                let count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM nodes WHERE ip = ?1 AND port = ?2",
                    rusqlite::params![node.ip, node.port],
                    |row| row.get(0)
                ).unwrap_or(0);
                
                if count == 0 {
                    let mut stmt = conn.prepare(
                        "INSERT INTO nodes (ip, port, ssh_port, group_name, auth_type, auth_data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
                    ).unwrap();
                    let secret = crypto::encrypt_data(&node.auth_secret);
                    let _ = stmt.execute(rusqlite::params![
                        node.ip,
                        node.port,
                        node.ssh_port,
                        node.group_name,
                        node.auth_type,
                        secret,
                    ]);
                }
            }
        }
        
        Some(Arc::new(database))
    } else {
        None
    };

    let mut initial_data = SystemData::default();
    initial_data.db = db;
    let app_state = Arc::new(RwLock::new(initial_data));
    
    // Spawn collector
    let collector_state = app_state.clone();
    std::thread::spawn(move || {
        collector_loop(collector_state);
    });

    // Spawn Web Server
    let web_state = app_state.clone();
    tokio::spawn(async move {
        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);
            
        let app = Router::new()
            .route("/", get(serve_html))
            .route("/api/login", post(api_login))
            .route("/api/change_password", post(api_change_password))
            .route("/api/metrics", get(api_metrics))
            .route("/api/nodes", get(api_nodes))
            .route("/api/nodes/{id}", axum::routing::delete(api_delete_node).put(api_update_node))
            .route("/api/deploy", post(api_deploy))
            .layer(middleware::from_fn(auth_middleware))
            .layer(cors)
            .with_state(web_state);
            
        let port = config.server.port;
        let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await.unwrap();
        axum::serve(listener, app).await.unwrap();
    });

    if args.agent_only {
        // Block forever if agent only
        std::future::pending::<()>().await;
        return Ok(());
    }

    // Setup TUI
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let tick_rate = Duration::from_millis(100); // UI responsive tick
    let mut last_tick = Instant::now();

    loop {
        let data = {
            let lock = app_state.read().unwrap();
            lock.clone()
        };
        
        terminal.draw(|f| ui(f, &data))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if crossterm::event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if let KeyCode::Char('q') = key.code {
                    break;
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
        }
    }

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
    )?;
    terminal.show_cursor()?;

    Ok(())
}

fn ui(f: &mut ratatui::Frame, data: &SystemData) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Percentage(50),
                Constraint::Percentage(50),
            ]
            .as_ref(),
        )
        .split(f.size());

    // 1. CPU
    let cpu_gauge = Gauge::default()
        .block(Block::default().title("CPU Usage (press 'q' to quit) - Web UI at http://localhost:3000").borders(Borders::ALL))
        .gauge_style(Style::default().fg(Color::Yellow))
        .percent(data.cpu_usage.clamp(0.0, 100.0) as u16);
    f.render_widget(cpu_gauge, chunks[0]);

    // 2. Memory
    let mem_percent = if data.mem_total > 0 {
        ((data.mem_used as f64 / data.mem_total as f64) * 100.0) as u16
    } else {
        0
    };
    let mem_gauge = Gauge::default()
        .block(Block::default().title(format!("Memory Usage: {} MB / {} MB", data.mem_used / 1024 / 1024, data.mem_total / 1024 / 1024)).borders(Borders::ALL))
        .gauge_style(Style::default().fg(Color::Cyan))
        .percent(mem_percent.clamp(0, 100));
    f.render_widget(mem_gauge, chunks[1]);

    // 3. Network
    let mut net_rows = Vec::new();
    for net in &data.networks {
        net_rows.push(Row::new(vec![
            net.interface.clone(),
            net.ip.clone(),
            net.mac.clone(),
            format!("{} B/s", net.rx_bytes_per_sec),
            format!("{} B/s", net.tx_bytes_per_sec),
        ]));
    }
    let net_table = Table::new(
        net_rows,
        [Constraint::Percentage(20), Constraint::Percentage(20), Constraint::Percentage(20), Constraint::Percentage(20), Constraint::Percentage(20)],
    )
    .block(Block::default().title("Network Auto-Discovery & Usage").borders(Borders::ALL))
    .header(Row::new(vec!["Interface", "IP Address", "MAC Address", "RX (Receive)", "TX (Transmit)"]).style(Style::default().fg(Color::Magenta)));
    
    f.render_widget(net_table, chunks[2]);

    // 4. I/O (Disks)
    let mut io_rows = Vec::new();
    for disk in &data.disks {
        let type_str = match disk.disk_type {
            DiskType::NVMe => "NVMe",
            DiskType::SataSsd => "SSD",
            DiskType::Hdd => "HDD",
            DiskType::Unknown => "?",
        };
        let smart_str = match &disk.smart_data {
            Some(sd) => format!("{}{}", if sd.passed { "OK " } else { "FAIL " }, sd.temperature.map(|t| format!("{}C", t)).unwrap_or_default()),
            None => "-".to_string(),
        };
        io_rows.push(Row::new(vec![
            format!("{} ({})", disk.name, type_str),
            if disk.mount_point.is_empty() { "-".to_string() } else { disk.mount_point.clone() },
            format!("{} MB", disk.total_space / 1024 / 1024),
            format!("{} MB", disk.available_space / 1024 / 1024),
            format!("{} B/s", disk.read_bytes_per_sec),
            format!("{} B/s", disk.write_bytes_per_sec),
            smart_str,
        ]));
    }
    let io_table = Table::new(
        io_rows,
        [Constraint::Percentage(14), Constraint::Percentage(14), Constraint::Percentage(14), Constraint::Percentage(14), Constraint::Percentage(14), Constraint::Percentage(14), Constraint::Percentage(16)],
    )
    .block(Block::default().title("Disk I/O, Space & Health").borders(Borders::ALL))
    .header(Row::new(vec!["Disk(Type)", "Mount Point", "Total", "Avail Space", "Read IO", "Write IO", "SMART(Temp)"]).style(Style::default().fg(Color::Green)));
    
    f.render_widget(io_table, chunks[3]);
}
