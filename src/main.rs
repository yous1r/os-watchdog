mod data;

use axum::{routing::get, Router, response::{IntoResponse, Html}, Json};
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
    time::{Duration, Instant}
};
use sysinfo::{System, Networks, Disks};
use get_if_addrs::get_if_addrs;
use data::{SystemData, NetworkInfo, DiskInfo};

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
    
    let mut prev_diskstats = read_diskstats();
    
    loop {
        sys.refresh_all();
        networks.refresh_list();
        networks.refresh();
        disks.refresh_list();
        disks.refresh();
        
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
            
            final_disks.push(DiskInfo {
                name: name.clone(),
                mount_point,
                total_space,
                available_space,
                read_bytes_per_sec: read_diff,
                write_bytes_per_sec: write_diff,
            });
        }
        
        for disk in disks.iter() {
            let d_name = disk.name().to_string_lossy().to_string();
            if !final_disks.iter().any(|d| d_name.contains(&d.name)) {
                final_disks.push(DiskInfo {
                    name: d_name,
                    mount_point: disk.mount_point().to_string_lossy().to_string(),
                    total_space: disk.total_space(),
                    available_space: disk.available_space(),
                    read_bytes_per_sec: 0,
                    write_bytes_per_sec: 0,
                });
            }
        }

        {
            let mut data = state.write().unwrap();
            data.cpu_usage = sys.global_cpu_info().cpu_usage();
            data.mem_total = sys.total_memory();
            data.mem_used = sys.used_memory();
            data.networks = net_info;
            data.disks = final_disks;
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let app_state = Arc::new(RwLock::new(SystemData::default()));
    
    // Spawn collector
    let collector_state = app_state.clone();
    std::thread::spawn(move || {
        collector_loop(collector_state);
    });

    // Spawn Web Server
    let web_state = app_state.clone();
    tokio::spawn(async move {
        let app = Router::new()
            .route("/", get(serve_html))
            .route("/api/metrics", get(api_metrics))
            .with_state(web_state);
            
        let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
        axum::serve(listener, app).await.unwrap();
    });

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
        io_rows.push(Row::new(vec![
            disk.name.clone(),
            if disk.mount_point.is_empty() { "-".to_string() } else { disk.mount_point.clone() },
            format!("{} MB", disk.total_space / 1024 / 1024),
            format!("{} MB", disk.available_space / 1024 / 1024),
            format!("{} B/s", disk.read_bytes_per_sec),
            format!("{} B/s", disk.write_bytes_per_sec),
        ]));
    }
    let io_table = Table::new(
        io_rows,
        [Constraint::Percentage(16), Constraint::Percentage(16), Constraint::Percentage(16), Constraint::Percentage(16), Constraint::Percentage(16), Constraint::Percentage(16)],
    )
    .block(Block::default().title("Disk I/O & Space").borders(Borders::ALL))
    .header(Row::new(vec!["Disk/Dev", "Mount Point", "Total Space", "Avail Space", "Read IO", "Write IO"]).style(Style::default().fg(Color::Green)));
    
    f.render_widget(io_table, chunks[3]);
}
