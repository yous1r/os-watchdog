use serde::Serialize;
use std::sync::Arc;
use crate::db::Db;

#[derive(Clone, Default, Serialize)]
pub struct SystemData {
    pub cpu_usage: f32,
    pub mem_total: u64,
    pub mem_used: u64,
    pub power_w: Option<f64>,
    pub temperatures: Vec<ComponentInfo>,
    pub networks: Vec<NetworkInfo>,
    pub disks: Vec<DiskInfo>,
    #[serde(skip)]
    pub db: Option<Arc<Db>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentInfo {
    pub label: String,
    pub temperature: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct NetworkInfo {
    pub interface: String,
    pub ip: String,
    pub mac: String,
    pub rx_bytes_per_sec: u64,
    pub tx_bytes_per_sec: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum DiskType {
    NVMe,
    SataSsd,
    Hdd,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct SmartData {
    pub passed: bool,
    pub temperature: Option<i64>,
    pub power_on_hours: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiskInfo {
    pub name: String,
    pub mount_point: String,
    pub disk_type: DiskType,
    pub smart_data: Option<SmartData>,
    pub total_space: u64,
    pub available_space: u64,
    pub read_bytes_per_sec: u64,
    pub write_bytes_per_sec: u64,
}
