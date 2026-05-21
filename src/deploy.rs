use serde::{Deserialize, Serialize};
use ssh2::{Session, Prompt, KeyboardInteractivePrompt};
use std::io::Read;
use std::io::Write;
use std::net::TcpStream;
use std::path::Path;
use std::env;
use std::fs::File;

struct PasswordPrompt(String);
impl KeyboardInteractivePrompt for PasswordPrompt {
    fn prompt<'a>(
        &mut self,
        _username: &str,
        _instruction: &str,
        prompts: &[Prompt<'a>]
    ) -> Vec<String> {
        prompts.iter().map(|_| self.0.clone()).collect()
    }
}

#[derive(Deserialize)]
pub struct DeployRequest {
    pub ip: String,
    pub port: u16,
    pub ssh_port: u16,
    pub group_name: Option<String>,
    pub password: Option<String>,
    pub private_key: Option<String>,
}

#[derive(Serialize)]
pub struct DeployResponse {
    pub success: bool,
    pub message: String,
}

pub async fn deploy_agent(req: DeployRequest) -> DeployResponse {
    // 1. Connect
    let tcp = match TcpStream::connect(format!("{}:{}", req.ip, req.ssh_port)) {
        Ok(t) => t,
        Err(e) => return DeployResponse { success: false, message: format!("Failed to connect: {}", e) },
    };
    let mut sess = match Session::new() {
        Ok(s) => s,
        Err(e) => return DeployResponse { success: false, message: format!("Failed to create SSH session: {}", e) },
    };
    sess.set_tcp_stream(tcp);
    if let Err(e) = sess.handshake() {
        return DeployResponse { success: false, message: format!("SSH handshake failed: {}", e) };
    }

    // 2. Authenticate
    if let Some(pw) = &req.password {
        let mut auth_ok = false;
        
        // Try standard password auth
        if sess.userauth_password("root", pw).is_ok() {
            auth_ok = true;
        } else {
            // Fallback to keyboard-interactive
            let mut promptr = PasswordPrompt(pw.clone());
            if sess.userauth_keyboard_interactive("root", &mut promptr).is_ok() {
                auth_ok = true;
            }
        }

        if !auth_ok {
            return DeployResponse { 
                success: false, 
                message: "Authentication failed. Please ensure the password is correct, AND your server's /etc/ssh/sshd_config has 'PermitRootLogin yes' and 'PasswordAuthentication yes' (or KbdInteractive).".to_string() 
            };
        }
    } else if let Some(key) = &req.private_key {
        let temp_dir = std::env::temp_dir();
        let key_path = temp_dir.join(format!(".os_watchdog_key_{}", std::process::id()));
        if let Err(e) = std::fs::write(&key_path, key) {
            return DeployResponse { success: false, message: format!("Failed to write temp key: {}", e) };
        }
        
        // set 600 permissions
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(mut perms) = std::fs::metadata(&key_path).map(|m| m.permissions()) {
                perms.set_mode(0o600);
                let _ = std::fs::set_permissions(&key_path, perms);
            }
        }

        let auth_res = sess.userauth_pubkey_file("root", None, &key_path, None);
        let _ = std::fs::remove_file(&key_path);

        if auth_res.is_err() {
            return DeployResponse { 
                success: false, 
                message: "Authentication failed using private key. Please check the key format and SSH server settings.".to_string() 
            };
        }
    } else {
        return DeployResponse { success: false, message: "No password or private key provided".to_string() };
    }

    if !sess.authenticated() {
        return DeployResponse { success: false, message: "Authentication failed".to_string() };
    }

    // Helper to run command
    let run_cmd = |sess: &Session, cmd: &str| -> Result<String, String> {
        let mut channel = sess.channel_session().map_err(|e| e.to_string())?;
        channel.exec(cmd).map_err(|e| e.to_string())?;
        let mut s = String::new();
        channel.read_to_string(&mut s).map_err(|e| e.to_string())?;
        channel.wait_close().map_err(|e| e.to_string())?;
        Ok(s)
    };

    // 3. User Creation
    let _ = run_cmd(&sess, "useradd -m -s /bin/bash os-watchdog");

    // 4. Sudo Config
    let _ = run_cmd(&sess, "usermod -aG sudo os-watchdog || usermod -aG wheel os-watchdog");
    let _ = run_cmd(&sess, "echo 'os-watchdog ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/os-watchdog");

    // 5. Environment Check (Debian vs CentOS)
    let setup_cmd = "
if command -v apt-get >/dev/null 2>&1; then
    apt-get update && apt-get install -y smartmontools
elif command -v yum >/dev/null 2>&1; then
    yum install -y smartmontools
elif command -v dnf >/dev/null 2>&1; then
    dnf install -y smartmontools
fi
";
    if let Err(e) = run_cmd(&sess, setup_cmd) {
        return DeployResponse { success: false, message: format!("Env setup failed: {}", e) };
    }

    // 6. Binary Distribution
    let exe_path = match env::current_exe() {
        Ok(p) => p,
        Err(e) => return DeployResponse { success: false, message: format!("Could not find current exe: {}", e) },
    };
    
    let exe_size = std::fs::metadata(&exe_path).map(|m| m.len()).unwrap_or(0);
    
    // Stop service if exists
    let _ = run_cmd(&sess, "systemctl stop os-watchdog");
    
    // SCP
    match sess.scp_send(Path::new("/usr/local/bin/os-watchdog"), 0o755, exe_size, None) {
        Ok(mut remote_file) => {
            if let Ok(mut local_file) = File::open(&exe_path) {
                let mut buffer = Vec::new();
                if local_file.read_to_end(&mut buffer).is_ok() {
                    let _ = remote_file.write_all(&buffer);
                }
            }
            let _ = remote_file.send_eof();
            let _ = remote_file.wait_eof();
            let _ = remote_file.close();
            let _ = remote_file.wait_close();
        },
        Err(e) => return DeployResponse { success: false, message: format!("SCP failed: {}", e) },
    }

    // 7. Config Setup
    let _ = run_cmd(&sess, "mkdir -p /etc/os-watchdog");
    let config_yaml = format!(
        "server:\n  port: {}\nnodes: []\n",
        req.port
    );
    let _ = run_cmd(&sess, &format!("cat << 'EOF' > /etc/os-watchdog/config.yaml\n{}\nEOF\n", config_yaml));
    let _ = run_cmd(&sess, "chown os-watchdog:os-watchdog /etc/os-watchdog/config.yaml");

    // 8. Service Setup
    let service_file = r#"
[Unit]
Description=OS Watchdog Agent
After=network.target

[Service]
Type=simple
User=os-watchdog
ExecStart=/usr/local/bin/os-watchdog --agent-only -c /etc/os-watchdog/config.yaml
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
"#;
    let _ = run_cmd(&sess, &format!("cat << 'EOF' > /etc/systemd/system/os-watchdog.service\n{}\nEOF\n", service_file));
    let _ = run_cmd(&sess, "systemctl daemon-reload");
    let _ = run_cmd(&sess, "systemctl enable os-watchdog");
    let _ = run_cmd(&sess, "systemctl start os-watchdog");

    DeployResponse {
        success: true,
        message: "Agent deployed and started successfully.".to_string()
    }
}
