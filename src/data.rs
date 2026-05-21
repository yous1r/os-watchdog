use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub struct SystemData {
    pub cpu_usage: f32,
    pub mem_total: u64,
    pub mem_used: u64,
    pub networks: Vec<NetworkInfo>,
    pub disks: Vec<DiskInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NetworkInfo {
    pub interface: String,
    pub ip: String,
    pub mac: String,
    pub rx_bytes_per_sec: u64,
    pub tx_bytes_per_sec: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiskInfo {
    pub name: String,
    pub mount_point: String,
    pub total_space: u64,
    pub available_space: u64,
    pub read_bytes_per_sec: u64,
    pub write_bytes_per_sec: u64,
}
